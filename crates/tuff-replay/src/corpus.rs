//! Compact text form of a legitimate client's serverbound movement stream,
//! recorded from a real vanilla client and replayed through the checks.
//!
//! A lane is a name, the blocks the client stood on (`fill` lines, relative
//! to the lane origin) and the packets it sent, positions relative to the
//! same origin.

use anyhow::{Context, Result, bail};
use tuff_core::protocol::play::{Movement, Serverbound};
use tuff_world_model::{BlockPos, Face};

#[derive(Clone, Debug, PartialEq)]
pub struct Fill {
    pub from: [i32; 3],
    pub to: [i32; 3],
    pub state: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Lane {
    pub name: String,
    pub spawn: Option<[f64; 3]>,
    pub fills: Vec<Fill>,
    pub packets: Vec<Serverbound>,
}

pub fn encode_packet(packet: &Serverbound, origin: [f64; 3]) -> Option<String> {
    Some(match packet {
        Serverbound::Move(m) => {
            let pos = match m.position {
                Some([x, y, z]) => format!(
                    "{:?} {:?} {:?}",
                    x - origin[0],
                    y - origin[1],
                    z - origin[2]
                ),
                None => "- - -".to_owned(),
            };
            let rot = match m.rotation {
                Some((yaw, pitch)) => format!("{yaw:?} {pitch:?}"),
                None => "- -".to_owned(),
            };
            format!("M {pos} {rot} {}", u8::from(m.on_ground))
        }
        Serverbound::PlayerInput(bits) => format!("I {bits}"),
        Serverbound::PlayerCommand(action) => format!("C {action}"),
        Serverbound::ClientTickEnd => "T".to_owned(),
        Serverbound::MoveVehicle {
            position: [x, y, z],
            yaw,
            pitch,
            on_ground,
        } => format!(
            "V {:?} {:?} {:?} {yaw:?} {pitch:?} {}",
            x - origin[0],
            y - origin[1],
            z - origin[2],
            u8::from(*on_ground)
        ),
        Serverbound::UseItem { off_hand, sequence } => {
            format!("U {} {sequence}", u8::from(*off_hand))
        }
        Serverbound::UseItemOn {
            hand,
            position,
            face,
            cursor: [cx, cy, cz],
            sequence,
        } => {
            let [ox, oy, oz] = block_origin(origin);
            format!(
                "P {hand} {} {} {} {} {cx:?} {cy:?} {cz:?} {sequence}",
                position.x - ox,
                position.y - oy,
                position.z - oz,
                face.id()
            )
        }
        _ => return None,
    })
}

fn block_origin(origin: [f64; 3]) -> [i32; 3] {
    origin.map(|v| v.floor() as i32)
}

fn optional<T: std::str::FromStr>(token: &str) -> Result<Option<T>> {
    if token == "-" {
        return Ok(None);
    }
    token
        .parse()
        .map(Some)
        .map_err(|_| anyhow::anyhow!("bad number {token:?}"))
}

fn decode_packet(line: &str, origin: [f64; 3]) -> Result<Serverbound> {
    let tokens: Vec<&str> = line.split_whitespace().collect();
    Ok(match tokens.as_slice() {
        ["M", x, y, z, yaw, pitch, ground] => {
            let position = match (
                optional::<f64>(x)?,
                optional::<f64>(y)?,
                optional::<f64>(z)?,
            ) {
                (Some(x), Some(y), Some(z)) => Some([x + origin[0], y + origin[1], z + origin[2]]),
                _ => None,
            };
            let rotation = match (optional::<f32>(yaw)?, optional::<f32>(pitch)?) {
                (Some(yaw), Some(pitch)) => Some((yaw, pitch)),
                _ => None,
            };
            Serverbound::Move(Movement {
                horizontal_collision: false,
                position,
                rotation,
                on_ground: *ground == "1",
            })
        }
        ["I", bits] => Serverbound::PlayerInput(bits.parse()?),
        ["C", action] => Serverbound::PlayerCommand(action.parse()?),
        ["T"] => Serverbound::ClientTickEnd,
        ["V", x, y, z, yaw, pitch, ground] => Serverbound::MoveVehicle {
            position: [
                x.parse::<f64>()? + origin[0],
                y.parse::<f64>()? + origin[1],
                z.parse::<f64>()? + origin[2],
            ],
            yaw: yaw.parse()?,
            pitch: pitch.parse()?,
            on_ground: *ground == "1",
        },
        ["U", hand, sequence] => Serverbound::UseItem {
            off_hand: *hand == "1",
            sequence: sequence.parse()?,
        },
        ["P", hand, x, y, z, face, cx, cy, cz, sequence] => {
            let [ox, oy, oz] = block_origin(origin);
            Serverbound::UseItemOn {
                hand: hand.parse()?,
                position: BlockPos {
                    x: x.parse::<i32>()? + ox,
                    y: y.parse::<i32>()? + oy,
                    z: z.parse::<i32>()? + oz,
                },
                face: Face::from_id(face.parse()?)?,
                cursor: [cx.parse()?, cy.parse()?, cz.parse()?],
                sequence: sequence.parse()?,
            }
        }
        _ => bail!("unreadable corpus line {line:?}"),
    })
}

impl Lane {
    pub fn render(&self, origin: [f64; 3]) -> String {
        let mut out = format!("lane {}\n", self.name);
        if let Some([x, y, z]) = self.spawn {
            out.push_str(&format!(
                "spawn {:?} {:?} {:?}\n",
                x - origin[0],
                y - origin[1],
                z - origin[2]
            ));
        }
        for f in &self.fills {
            let [a, b, c] = f.from;
            let [d, e, g] = f.to;
            out.push_str(&format!("fill {a} {b} {c} {d} {e} {g} {}\n", f.state));
        }
        for packet in &self.packets {
            if let Some(line) = encode_packet(packet, origin) {
                out.push_str(&line);
                out.push('\n');
            }
        }
        out
    }

    /// Parses lanes; packet positions are shifted by `origin`.
    pub fn parse_all(text: &str, origin: [f64; 3]) -> Result<Vec<Lane>> {
        let mut lanes: Vec<Lane> = Vec::new();
        for (n, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Some(name) = line.strip_prefix("lane ") {
                lanes.push(Lane {
                    name: name.to_owned(),
                    ..Lane::default()
                });
                continue;
            }
            let lane = lanes.last_mut().context("packet before a lane header")?;
            if let Some(rest) = line.strip_prefix("spawn ") {
                let v: Vec<f64> = rest
                    .split_whitespace()
                    .map(str::parse)
                    .collect::<Result<_, _>>()?;
                let [x, y, z] = v[..] else {
                    bail!("bad spawn line {line:?}");
                };
                lane.spawn = Some([x + origin[0], y + origin[1], z + origin[2]]);
            } else if let Some(rest) = line.strip_prefix("fill ") {
                let mut it = rest.splitn(7, ' ');
                let mut num = || -> Result<i32> { Ok(it.next().context("short fill")?.parse()?) };
                let from = [num()?, num()?, num()?];
                let to = [num()?, num()?, num()?];
                let state = it.next().context("fill without block")?.to_owned();
                lane.fills.push(Fill { from, to, state });
            } else {
                lane.packets
                    .push(decode_packet(line, origin).with_context(|| format!("line {}", n + 1))?);
            }
        }
        Ok(lanes)
    }

    /// The first absolute position the client reported.
    pub fn start(&self) -> Option<[f64; 3]> {
        self.packets.iter().find_map(|p| match p {
            Serverbound::Move(Movement {
                position: Some(pos),
                ..
            }) => Some(*pos),
            _ => None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn a_lane_round_trips_relative_to_any_origin() {
        let lane = Lane {
            spawn: Some([0.5, 1.0, 0.5]),
            name: "walk".into(),
            fills: vec![Fill {
                from: [-3, 0, 0],
                to: [40, 0, 0],
                state: "minecraft:stone_slab[type=bottom]".into(),
            }],
            packets: vec![
                Serverbound::PlayerInput(1),
                Serverbound::PlayerCommand(1),
                Serverbound::Move(Movement {
                    horizontal_collision: false,
                    position: Some([105.5, 71.0, -20.25]),
                    rotation: Some((-90.0, 1.5)),
                    on_ground: true,
                }),
                Serverbound::Move(Movement {
                    horizontal_collision: false,
                    position: None,
                    rotation: Some((10.0, 0.0)),
                    on_ground: false,
                }),
                Serverbound::MoveVehicle {
                    position: [101.5, 70.5, -19.0],
                    yaw: 45.0,
                    pitch: 2.5,
                    on_ground: false,
                },
                Serverbound::UseItem {
                    off_hand: true,
                    sequence: 7,
                },
                Serverbound::UseItemOn {
                    hand: 0,
                    position: BlockPos {
                        x: 104,
                        y: 69,
                        z: -21,
                    },
                    face: Face::Up,
                    cursor: [0.25, 1.0, 0.5],
                    sequence: 8,
                },
                Serverbound::ClientTickEnd,
            ],
        };
        let text = lane.render([100.0, 70.0, -20.0]);
        let back = Lane::parse_all(&text, [100.0, 70.0, -20.0]).unwrap();
        assert_eq!(back, vec![lane.clone()]);
        let shifted = Lane::parse_all(&text, [0.0, 0.0, 0.0]).unwrap();
        assert_eq!(shifted[0].start(), Some([5.5, 1.0, -0.25]));
        assert!(Lane::parse_all("M 1 2 3 - - 1", [0.0; 3]).is_err());
    }
}
