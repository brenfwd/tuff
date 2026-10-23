//! The `particle` argument: a particle type and the options vanilla reads
//! from the SNBT that may follow it, kept in the form the client reads.

use tuff_vanilla_data::registry::ParticleType;

use super::CommandError;
use super::args::{read_identifier, read_nbt_compound};
use super::reader::Reader;
use crate::protocol::codec::WriteExt;
use tuff_item::item::ItemPicture;
use tuff_nbt::{Compound, Tag};
use tuff_wire::PacketWriter;

#[derive(Debug, Clone, PartialEq)]
pub struct ParticleInput {
    pub kind: ParticleType,
    /// What follows the type id in a particle packet.
    pub options: Vec<u8>,
}

impl ParticleInput {
    pub fn name(&self) -> String {
        format!("minecraft:{}", self.kind.name())
    }
}

fn invalid(message: &str) -> CommandError {
    CommandError::dynamic("particle.invalidOptions", [message.to_owned()])
}

pub fn parse(reader: &mut Reader<'_>) -> Result<ParticleInput, CommandError> {
    let start = reader.cursor();
    let id = read_identifier(reader)?;
    let Some(kind) = ParticleType::from_name(&id) else {
        reader.set_cursor(start);
        return Err(CommandError::dynamic("particle.notFound", [id]).at(reader));
    };
    let data = if reader.can_read() && reader.peek_is('{') {
        read_nbt_compound(reader)?
    } else {
        Compound::new()
    };
    let mut w = PacketWriter::new();
    encode(kind, &data, &mut w)?;
    Ok(ParticleInput {
        kind,
        options: w.into_bytes().to_vec(),
    })
}

/// The packet options of `kind` from its options compound, checked the
/// way `/particle` checks them.
pub fn encode_options(kind: ParticleType, data: &Compound) -> Result<Vec<u8>, CommandError> {
    let mut w = PacketWriter::new();
    encode(kind, data, &mut w)?;
    Ok(w.into_bytes().to_vec())
}

fn number(tag: &Tag) -> Option<f64> {
    match tag {
        Tag::Byte(v) => Some(f64::from(*v)),
        Tag::Short(v) => Some(f64::from(*v)),
        Tag::Int(v) => Some(f64::from(*v)),
        Tag::Long(v) => Some(*v as f64),
        Tag::Float(v) => Some(f64::from(*v)),
        Tag::Double(v) => Some(*v),
        _ => None,
    }
}

fn float(data: &Compound, key: &str, default: Option<f32>) -> Result<f32, CommandError> {
    match data.get(key) {
        Some(tag) => number(tag)
            .map(|v| v as f32)
            .ok_or_else(|| invalid(&format!("Not a number at {key}"))),
        None => default.ok_or_else(|| invalid(&format!("Missing field {key}"))),
    }
}

fn int(data: &Compound, key: &str, default: Option<i32>) -> Result<i32, CommandError> {
    match data.get(key) {
        Some(Tag::Byte(v)) => Ok(i32::from(*v)),
        Some(Tag::Short(v)) => Ok(i32::from(*v)),
        Some(Tag::Int(v)) => Ok(*v),
        Some(_) => Err(invalid(&format!("Not an integer at {key}"))),
        None => default.ok_or_else(|| invalid(&format!("Missing field {key}"))),
    }
}

fn positive(data: &Compound, key: &str) -> Result<i32, CommandError> {
    let value = int(data, key, None)?;
    if value < 1 {
        return Err(invalid(&format!("Value must be positive: {value}")));
    }
    Ok(value)
}

fn scale(data: &Compound) -> Result<f32, CommandError> {
    let value = float(data, "scale", None)?;
    if !(0.01..=4.0).contains(&value) {
        return Err(invalid(&format!(
            "Value must be within range [0.01;4.0]: {value}"
        )));
    }
    Ok(value)
}

fn pack(a: f64, r: f64, g: f64, b: f64) -> i32 {
    (channel(a) << 24) | (channel(r) << 16) | (channel(g) << 8) | channel(b)
}

fn channel(value: f64) -> i32 {
    (value.clamp(0.0, 1.0) * 255.0).round() as i32
}

/// A colour as a packed integer, or as a list of 3 (RGB) or 4 (RGBA)
/// floats; `alpha` makes the list form need 4.
fn color(tag: Option<&Tag>, key: &str, alpha: bool) -> Result<i32, CommandError> {
    match tag {
        Some(Tag::Int(v)) => Ok(*v),
        Some(Tag::List(list)) => {
            let values: Option<Vec<f64>> = list.iter().map(number).collect();
            let values = values.ok_or_else(|| invalid(&format!("Not a number at {key}")))?;
            match (values.as_slice(), alpha) {
                ([r, g, b], false) => Ok(pack(1.0, *r, *g, *b)),
                ([r, g, b, a], true) => Ok(pack(*a, *r, *g, *b)),
                _ => Err(invalid(&format!("Invalid color at {key}"))),
            }
        }
        Some(_) => Err(invalid(&format!("Invalid color at {key}"))),
        None => Err(invalid(&format!("Missing field {key}"))),
    }
}

fn block_state(tag: Option<&Tag>) -> Result<u32, CommandError> {
    let text = match tag {
        Some(Tag::String(name)) => name.clone(),
        Some(Tag::Compound(state)) => {
            let name = state
                .get_str("Name")
                .ok_or_else(|| invalid("Missing field Name"))?;
            let mut text = name.to_owned();
            if let Some(properties) = state.get_compound("Properties") {
                let pairs: Vec<String> = properties
                    .iter()
                    .filter_map(|(k, v)| Some(format!("{k}={}", v.as_str()?)))
                    .collect();
                if !pairs.is_empty() {
                    text = format!("{text}[{}]", pairs.join(","));
                }
            }
            text
        }
        _ => return Err(invalid("Missing field block_state")),
    };
    let mut reader = Reader::new(&text);
    let input = super::block::parse_block_state(&mut reader)
        .map_err(|_| invalid(&format!("Unknown block state {text}")))?;
    if reader.can_read() {
        return Err(invalid(&format!("Unknown block state {text}")));
    }
    Ok(input.state.id())
}

fn encode(kind: ParticleType, data: &Compound, w: &mut PacketWriter) -> Result<(), CommandError> {
    use ParticleType as P;
    match kind {
        P::Block | P::BlockMarker | P::FallingDust | P::DustPillar | P::BlockCrumble => {
            w.write_var_int(block_state(data.get("block_state"))? as i32);
        }
        P::EntityEffect | P::TintedLeaves | P::Flash => {
            w.write_int(color(data.get("color"), "color", true)?);
        }
        P::Dust => {
            w.write_int(color(data.get("color"), "color", false)?);
            w.write_float(scale(data)?);
        }
        P::DustColorTransition => {
            w.write_int(color(data.get("from_color"), "from_color", false)?);
            w.write_int(color(data.get("to_color"), "to_color", false)?);
            w.write_float(scale(data)?);
        }
        P::Item => {
            let Some(Tag::Compound(item)) = data.get("item") else {
                return Err(invalid("Missing field item"));
            };
            let stack = ItemPicture::from_nbt(item).ok_or_else(|| invalid("Invalid item"))?;
            w.write_slot(Some(stack.view()));
        }
        P::Geyser | P::GeyserPlume => w.write_int(positive(data, "water_blocks")?),
        P::GeyserBase | P::GeyserPoof => {
            w.write_int(positive(data, "water_blocks")?);
            w.write_float(float(data, "burst_impulse_base", None)?);
        }
        P::Shriek => w.write_var_int(int(data, "delay", None)?),
        P::SculkCharge => w.write_float(float(data, "roll", None)?),
        P::DragonBreath => w.write_float(float(data, "power", Some(1.0))?),
        P::Effect | P::InstantEffect => {
            let rgb = match data.get("color") {
                Some(tag) => color(Some(tag), "color", false)?,
                None => -1,
            };
            w.write_int(rgb);
            w.write_float(float(data, "power", Some(1.0))?);
        }
        P::Trail => {
            let Some(Tag::List(target)) = data.get("target") else {
                return Err(invalid("Missing field target"));
            };
            let values: Option<Vec<f64>> = target.iter().map(number).collect();
            let Some([x, y, z]) = values.as_deref().and_then(|v| <[f64; 3]>::try_from(v).ok())
            else {
                return Err(invalid("Invalid target"));
            };
            for c in [x, y, z] {
                w.write_double(c);
            }
            w.write_int(color(data.get("color"), "color", false)?);
            w.write_var_int(positive(data, "duration")?);
        }
        P::Vibration => {
            let Some(Tag::Compound(destination)) = data.get("destination") else {
                return Err(invalid("Missing field destination"));
            };
            if destination.get_str("type") != Some("minecraft:block")
                && destination.get_str("type") != Some("block")
            {
                return Err(invalid("Unsupported position source"));
            }
            let Some(Tag::List(pos)) = destination.get("pos") else {
                return Err(invalid("Missing field pos"));
            };
            let values: Option<Vec<i32>> =
                pos.iter().map(|t| number(t).map(|v| v as i32)).collect();
            let Some([x, y, z]) = values.as_deref().and_then(|v| <[i32; 3]>::try_from(v).ok())
            else {
                return Err(invalid("Invalid pos"));
            };
            w.write_var_int(0);
            w.write_position(tuff_world_model::BlockPos { x, y, z });
            w.write_var_int(int(data, "arrival_in_ticks", None)?);
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(input: &str) -> Result<Vec<u8>, String> {
        parse(&mut Reader::new(input))
            .map(|p| p.options)
            .map_err(|e| format!("{e:?}"))
    }

    #[tuff_core::watchdog]
    #[test]
    fn simple_particles_carry_no_options() {
        assert_eq!(options("minecraft:flame"), Ok(Vec::new()));
        assert_eq!(options("flame{}"), Ok(Vec::new()));
        assert!(options("minecraft:nothing").is_err());
    }

    #[tuff_core::watchdog]
    #[test]
    fn dust_reads_color_lists_and_checks_scale() {
        let list = options("dust{color:[1.0,0.0,0.0],scale:1.5}").unwrap();
        assert_eq!(list[..4], 0xFFFF0000u32.to_be_bytes());
        assert_eq!(list[4..], 1.5f32.to_be_bytes());
        assert_eq!(
            options("dust{color:16711680,scale:1.5}").unwrap()[..4],
            16711680i32.to_be_bytes()
        );
        assert!(options("dust{color:[1.0,0.0,0.0],scale:9}").is_err());
        assert!(options("dust{scale:1}").is_err());
    }

    #[tuff_core::watchdog]
    #[test]
    fn block_item_and_trail_options_encode() {
        assert!(options("block{block_state:\"minecraft:stone\"}").is_ok());
        assert!(
            options("block{block_state:{Name:\"minecraft:oak_log\",Properties:{axis:\"x\"}}}")
                .is_ok()
        );
        assert!(options("block{block_state:\"minecraft:nothing\"}").is_err());
        assert!(options("item{item:{id:\"minecraft:stick\",count:1}}").is_ok());
        assert!(options("trail{target:[1.0,2.0,3.0],color:255,duration:10}").is_ok());
        assert!(options("trail{target:[1.0,2.0],color:255,duration:10}").is_err());
        assert!(
            options(
                "vibration{destination:{type:\"minecraft:block\",pos:[1,2,3]},arrival_in_ticks:20}"
            )
            .is_ok()
        );
        assert!(options("shriek{delay:5}").is_ok());
        assert!(options("geyser{water_blocks:0}").is_err());
    }
}
