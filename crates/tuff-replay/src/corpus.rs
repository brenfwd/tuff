//! Compact text form of a legitimate client's serverbound movement stream,
//! recorded from a real vanilla client and replayed through the checks.
//!
//! A lane is a name, the blocks the client stood on (`fill` lines, relative
//! to the lane origin) and the packets it sent, positions relative to the
//! same origin.
//!
//! Two forms exist: the text form (`render`/`parse_all`, used by unit tests
//! and debugging) and a lossless binary form for checked-in files. A binary
//! file is the magic `TCOR1` followed by length-prefixed lane records, so
//! records can be appended, split and dropped without re-encoding. In a
//! record, every packet is one opcode byte (kind in bits 0-2, flags above)
//! plus operands. Move and vehicle positions are coded as the signed
//! distance in ordered-float space from a linear extrapolation of the two
//! previous positions, rotations from the previous rotation, both as
//! zigzag varints, so the round trip is bit-exact.

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
        Serverbound::PaddleBoat { left, right } => {
            format!("B {} {}", u8::from(*left), u8::from(*right))
        }
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
        ["B", left, right] => Serverbound::PaddleBoat {
            left: *left == "1",
            right: *right == "1",
        },
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

pub const MAGIC: &[u8] = b"TCOR1";

const OP_MOVE: u8 = 0;
const OP_INPUT: u8 = 1;
const OP_COMMAND: u8 = 2;
const OP_TICK_END: u8 = 3;
const OP_VEHICLE: u8 = 4;
const OP_USE_ITEM: u8 = 5;
const OP_USE_ITEM_ON: u8 = 6;
const FLAG_A: u8 = 1 << 3;
const FLAG_B: u8 = 1 << 4;
const FLAG_GROUND: u8 = 1 << 5;

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn zigzag(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}

fn unzigzag(v: u64) -> i64 {
    ((v >> 1) as i64) ^ -((v & 1) as i64)
}

fn put_signed(out: &mut Vec<u8>, v: i64) {
    put_varint(out, zigzag(v));
}

fn ordered64(bits: u64) -> i64 {
    let i = bits as i64;
    if i < 0 { i ^ i64::MAX } else { i }
}

fn ordered32(bits: u32) -> i64 {
    let i = bits as i32;
    i64::from(if i < 0 { i ^ i32::MAX } else { i })
}

fn unordered32(v: i64) -> u32 {
    let i = v as i32;
    (if i < 0 { i ^ i32::MAX } else { i }) as u32
}

/// Previous positions and rotation, shared by the encoder and decoder so
/// both derive the same prediction.
#[derive(Default)]
struct Predictor {
    pos: [[f64; 3]; 2],
    seen: usize,
    rot: [f32; 2],
}

impl Predictor {
    fn predict(&self, axis: usize) -> f64 {
        match self.seen {
            0 => 0.0,
            1 => self.pos[0][axis],
            _ => 2.0 * self.pos[0][axis] - self.pos[1][axis],
        }
    }

    fn push_pos(&mut self, pos: [f64; 3]) {
        self.pos[1] = self.pos[0];
        self.pos[0] = pos;
        self.seen = (self.seen + 1).min(2);
    }

    fn put_pos(&mut self, out: &mut Vec<u8>, pos: [f64; 3]) {
        for (axis, v) in pos.iter().enumerate() {
            let delta =
                ordered64(v.to_bits()).wrapping_sub(ordered64(self.predict(axis).to_bits()));
            put_signed(out, delta);
        }
        self.push_pos(pos);
    }

    fn put_rot(&mut self, out: &mut Vec<u8>, rot: [f32; 2]) {
        for (prev, v) in self.rot.iter().zip(rot) {
            put_signed(out, ordered32(v.to_bits()) - ordered32(prev.to_bits()));
        }
        self.rot = rot;
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
}

impl<'a> Reader<'a> {
    fn byte(&mut self) -> Result<u8> {
        let (&b, rest) = self.bytes.split_first().context("truncated corpus")?;
        self.bytes = rest;
        Ok(b)
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if n > self.bytes.len() {
            bail!("truncated corpus");
        }
        let (head, rest) = self.bytes.split_at(n);
        self.bytes = rest;
        Ok(head)
    }

    fn varint(&mut self) -> Result<u64> {
        let mut v = 0u64;
        for shift in (0..70).step_by(7) {
            let b = self.byte()?;
            if shift == 63 && b > 1 {
                bail!("corpus varint overflow");
            }
            v |= u64::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                return Ok(v);
            }
        }
        bail!("corpus varint too long")
    }

    fn signed(&mut self) -> Result<i64> {
        Ok(unzigzag(self.varint()?))
    }

    fn int(&mut self) -> Result<i32> {
        i32::try_from(self.signed()?).context("corpus integer out of range")
    }

    fn count(&mut self, min_bytes_each: usize) -> Result<usize> {
        let n = usize::try_from(self.varint()?).context("corpus count")?;
        if n.saturating_mul(min_bytes_each) > self.bytes.len() {
            bail!("corpus count exceeds the remaining bytes");
        }
        Ok(n)
    }

    fn f64_raw(&mut self) -> Result<f64> {
        Ok(f64::from_bits(u64::from_le_bytes(
            self.take(8)?.try_into().unwrap(),
        )))
    }

    fn f32_raw(&mut self) -> Result<f32> {
        Ok(f32::from_bits(u32::from_le_bytes(
            self.take(4)?.try_into().unwrap(),
        )))
    }

    fn pos(&mut self, p: &mut Predictor) -> Result<[f64; 3]> {
        let mut out = [0.0; 3];
        for (axis, slot) in out.iter_mut().enumerate() {
            let delta = self.signed()?;
            let bits = ordered64(p.predict(axis).to_bits()).wrapping_add(delta);
            *slot = f64::from_bits(ordered64(bits as u64) as u64);
        }
        p.push_pos(out);
        Ok(out)
    }

    fn rot(&mut self, p: &mut Predictor) -> Result<[f32; 2]> {
        let mut out = [0.0; 2];
        for (slot, prev) in out.iter_mut().zip(p.rot) {
            let bits = ordered32(prev.to_bits()) + self.signed()?;
            *slot = f32::from_bits(unordered32(bits));
        }
        p.rot = out;
        Ok(out)
    }
}

fn put_packet(
    out: &mut Vec<u8>,
    p: &mut Predictor,
    packet: &Serverbound,
    origin: [f64; 3],
) -> bool {
    let rel = |pos: [f64; 3]| [pos[0] - origin[0], pos[1] - origin[1], pos[2] - origin[2]];
    match packet {
        Serverbound::Move(m) => {
            let mut op = OP_MOVE;
            if m.position.is_some() {
                op |= FLAG_A;
            }
            if m.rotation.is_some() {
                op |= FLAG_B;
            }
            if m.on_ground {
                op |= FLAG_GROUND;
            }
            out.push(op);
            if let Some(pos) = m.position {
                p.put_pos(out, rel(pos));
            }
            if let Some((yaw, pitch)) = m.rotation {
                p.put_rot(out, [yaw, pitch]);
            }
        }
        Serverbound::PlayerInput(bits) => {
            out.push(OP_INPUT);
            out.push(*bits);
        }
        Serverbound::PlayerCommand(action) => {
            out.push(OP_COMMAND);
            put_signed(out, i64::from(*action));
        }
        Serverbound::ClientTickEnd => out.push(OP_TICK_END),
        Serverbound::MoveVehicle {
            position,
            yaw,
            pitch,
            on_ground,
        } => {
            out.push(OP_VEHICLE | if *on_ground { FLAG_GROUND } else { 0 });
            p.put_pos(out, rel(*position));
            p.put_rot(out, [*yaw, *pitch]);
        }
        Serverbound::UseItem { off_hand, sequence } => {
            out.push(OP_USE_ITEM | if *off_hand { FLAG_A } else { 0 });
            put_signed(out, i64::from(*sequence));
        }
        Serverbound::UseItemOn {
            hand,
            position,
            face,
            cursor,
            sequence,
        } => {
            let [ox, oy, oz] = block_origin(origin);
            out.push(OP_USE_ITEM_ON);
            put_signed(out, i64::from(*hand));
            put_signed(out, i64::from(position.x - ox));
            put_signed(out, i64::from(position.y - oy));
            put_signed(out, i64::from(position.z - oz));
            put_signed(out, i64::from(face.id()));
            for c in cursor {
                out.extend_from_slice(&c.to_bits().to_le_bytes());
            }
            put_signed(out, i64::from(*sequence));
        }
        _ => return false,
    }
    true
}

fn read_packet(r: &mut Reader, p: &mut Predictor, origin: [f64; 3]) -> Result<Serverbound> {
    let op = r.byte()?;
    let abs = |pos: [f64; 3]| [pos[0] + origin[0], pos[1] + origin[1], pos[2] + origin[2]];
    let ground = op & FLAG_GROUND != 0;
    Ok(match op & 7 {
        OP_MOVE => {
            let position = if op & FLAG_A != 0 {
                Some(abs(r.pos(p)?))
            } else {
                None
            };
            let rotation = if op & FLAG_B != 0 {
                let [yaw, pitch] = r.rot(p)?;
                Some((yaw, pitch))
            } else {
                None
            };
            Serverbound::Move(Movement {
                horizontal_collision: false,
                position,
                rotation,
                on_ground: ground,
            })
        }
        OP_INPUT => Serverbound::PlayerInput(r.byte()?),
        OP_COMMAND => Serverbound::PlayerCommand(r.int()?),
        OP_TICK_END => Serverbound::ClientTickEnd,
        OP_VEHICLE => {
            let position = abs(r.pos(p)?);
            let [yaw, pitch] = r.rot(p)?;
            Serverbound::MoveVehicle {
                position,
                yaw,
                pitch,
                on_ground: ground,
            }
        }
        OP_USE_ITEM => Serverbound::UseItem {
            off_hand: op & FLAG_A != 0,
            sequence: r.int()?,
        },
        OP_USE_ITEM_ON => {
            let [ox, oy, oz] = block_origin(origin);
            let hand = r.int()?;
            let position = BlockPos {
                x: r.int()?.wrapping_add(ox),
                y: r.int()?.wrapping_add(oy),
                z: r.int()?.wrapping_add(oz),
            };
            let face = Face::from_id(r.int()?)?;
            let cursor = [r.f32_raw()?, r.f32_raw()?, r.f32_raw()?];
            Serverbound::UseItemOn {
                hand,
                position,
                face,
                cursor,
                sequence: r.int()?,
            }
        }
        _ => bail!("unknown corpus opcode {op:#x}"),
    })
}

impl Lane {
    /// One length-prefixed binary lane record; append to a file that starts
    /// with [`MAGIC`].
    pub fn encode_record(&self, origin: [f64; 3]) -> Vec<u8> {
        let mut body = Vec::new();
        put_varint(&mut body, self.name.len() as u64);
        body.extend_from_slice(self.name.as_bytes());
        match self.spawn {
            Some([x, y, z]) => {
                body.push(1);
                for v in [x - origin[0], y - origin[1], z - origin[2]] {
                    body.extend_from_slice(&v.to_bits().to_le_bytes());
                }
            }
            None => body.push(0),
        }
        put_varint(&mut body, self.fills.len() as u64);
        for f in &self.fills {
            for v in f.from.into_iter().chain(f.to) {
                put_signed(&mut body, i64::from(v));
            }
            put_varint(&mut body, f.state.len() as u64);
            body.extend_from_slice(f.state.as_bytes());
        }
        let mut packets = Vec::new();
        let mut predictor = Predictor::default();
        let mut count = 0u64;
        for packet in &self.packets {
            count += u64::from(put_packet(&mut packets, &mut predictor, packet, origin));
        }
        put_varint(&mut body, count);
        body.extend_from_slice(&packets);
        let mut out = Vec::with_capacity(body.len() + 5);
        put_varint(&mut out, body.len() as u64);
        out.extend_from_slice(&body);
        out
    }

    /// A whole binary corpus file holding `lanes`.
    pub fn encode_all(lanes: &[Lane], origin: [f64; 3]) -> Vec<u8> {
        let mut out = MAGIC.to_vec();
        for lane in lanes {
            out.extend_from_slice(&lane.encode_record(origin));
        }
        out
    }

    /// The lane records of a binary corpus file, undecoded.
    pub fn split_records(bytes: &[u8]) -> Result<Vec<&[u8]>> {
        let body = bytes.strip_prefix(MAGIC).context("bad corpus magic")?;
        let mut r = Reader { bytes: body };
        let mut records = Vec::new();
        while !r.bytes.is_empty() {
            let len = r.count(1)?;
            let start = body.len() - r.bytes.len() - varint_len(len as u64);
            r.take(len)?;
            let end = body.len() - r.bytes.len();
            records.push(&body[start..end]);
        }
        Ok(records)
    }

    /// Decodes one record from [`Lane::split_records`]; packet positions are
    /// shifted by `origin`.
    pub fn decode_record(record: &[u8], origin: [f64; 3]) -> Result<Lane> {
        let mut outer = Reader { bytes: record };
        let len = outer.count(1)?;
        let mut r = Reader {
            bytes: outer.take(len)?,
        };
        if !outer.bytes.is_empty() {
            bail!("trailing bytes after a corpus record");
        }
        let name_len = r.count(1)?;
        let name = String::from_utf8(r.take(name_len)?.to_vec())?;
        let spawn = match r.byte()? {
            0 => None,
            1 => {
                let [x, y, z] = [r.f64_raw()?, r.f64_raw()?, r.f64_raw()?];
                Some([x + origin[0], y + origin[1], z + origin[2]])
            }
            other => bail!("bad spawn flag {other}"),
        };
        let fill_count = r.count(7)?;
        let mut fills = Vec::with_capacity(fill_count);
        for _ in 0..fill_count {
            let from = [r.int()?, r.int()?, r.int()?];
            let to = [r.int()?, r.int()?, r.int()?];
            let state_len = r.count(1)?;
            let state = String::from_utf8(r.take(state_len)?.to_vec())?;
            fills.push(Fill { from, to, state });
        }
        let packet_count = r.count(1)?;
        let mut packets = Vec::with_capacity(packet_count);
        let mut predictor = Predictor::default();
        for _ in 0..packet_count {
            packets.push(read_packet(&mut r, &mut predictor, origin)?);
        }
        if !r.bytes.is_empty() {
            bail!("trailing bytes in lane {name:?}");
        }
        Ok(Lane {
            name,
            spawn,
            fills,
            packets,
        })
    }

    /// Decodes a binary corpus file.
    pub fn decode_all(bytes: &[u8], origin: [f64; 3]) -> Result<Vec<Lane>> {
        Self::split_records(bytes)?
            .into_iter()
            .map(|record| Self::decode_record(record, origin))
            .collect()
    }
}

fn varint_len(v: u64) -> usize {
    (64 - (v | 1).leading_zeros() as usize).div_ceil(7)
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

    fn all_kinds() -> Lane {
        let mv = |position, rotation, on_ground| {
            Serverbound::Move(Movement {
                horizontal_collision: false,
                position,
                rotation,
                on_ground,
            })
        };
        Lane {
            name: "kinds".into(),
            spawn: Some([0.5, -0.0, 5e-324]),
            fills: vec![Fill {
                from: [-3, 0, i32::MIN],
                to: [i32::MAX, 0, 0],
                state: "minecraft:stone".into(),
            }],
            packets: vec![
                mv(Some([1.5, 70.0, -2.25]), Some((-90.0, 1.5)), true),
                mv(Some([1.6, 70.0, -2.3]), None, false),
                mv(None, Some((f32::MAX, -f32::MIN_POSITIVE)), false),
                mv(None, None, true),
                mv(Some([-0.0, f64::MAX, f64::MIN]), Some((-0.0, 1e-45)), true),
                mv(Some([f64::MIN_POSITIVE, 5e-324, 1e300]), None, false),
                Serverbound::PlayerInput(255),
                Serverbound::PlayerCommand(-7),
                Serverbound::ClientTickEnd,
                Serverbound::MoveVehicle {
                    position: [3.0, 4.0, 5.0],
                    yaw: 12.5,
                    pitch: -3.0,
                    on_ground: true,
                },
                Serverbound::UseItem {
                    off_hand: true,
                    sequence: i32::MAX,
                },
                Serverbound::UseItem {
                    off_hand: false,
                    sequence: -1,
                },
                Serverbound::UseItemOn {
                    hand: 1,
                    position: BlockPos {
                        x: -4,
                        y: 9,
                        z: 100,
                    },
                    face: Face::North,
                    cursor: [0.0, 0.123_456_79, 1.0],
                    sequence: 3,
                },
            ],
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_binary_form_round_trips_every_packet_kind_bit_exactly() {
        let lane = all_kinds();
        let zero = [0.0; 3];
        let bytes = Lane::encode_all(std::slice::from_ref(&lane), zero);
        assert_eq!(Lane::decode_all(&bytes, zero).unwrap(), vec![lane.clone()]);
        let origin = [100.0, 70.0, -20.0];
        let bytes = Lane::encode_all(std::slice::from_ref(&lane), origin);
        let text = Lane::parse_all(&lane.render(origin), origin).unwrap();
        assert_eq!(Lane::decode_all(&bytes, origin).unwrap(), text);
        let two = Lane::encode_all(&[lane.clone(), lane.clone()], zero);
        assert_eq!(Lane::decode_all(&two, zero).unwrap().len(), 2);
        assert_eq!(Lane::split_records(&two).unwrap().len(), 2);
    }

    #[tuff_core::watchdog]
    #[test]
    fn hostile_binary_input_is_rejected_without_allocating() {
        let bytes = Lane::encode_all(&[all_kinds()], [0.0; 3]);
        for cut in 0..bytes.len() {
            assert!(
                Lane::decode_all(&bytes[..cut], [0.0; 3]).is_err() || cut == MAGIC.len(),
                "cut {cut}"
            );
        }
        assert!(Lane::decode_all(b"NOPE1", [0.0; 3]).is_err());
        let mut huge = MAGIC.to_vec();
        huge.extend_from_slice(&[0x0a, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x7f]);
        assert!(Lane::decode_all(&huge, [0.0; 3]).is_err());
        let mut many = MAGIC.to_vec();
        many.extend_from_slice(&[6, 0, 0, 0, 0xff, 0xff, 0x0f]);
        assert!(Lane::decode_all(&many, [0.0; 3]).is_err());
        let mut overlong = MAGIC.to_vec();
        overlong.extend_from_slice(&[0xff; 12]);
        assert!(Lane::decode_all(&overlong, [0.0; 3]).is_err());
    }
}
