//! The client half of a fake player: reads the frames the server sends and
//! answers only what a client must, peeking packet ids and decoding just
//! those few.

use bytes::Bytes;
use tokio::sync::watch;
use tuff_vanilla_data::packet::play::Clientbound as Id;

use super::Observed;
use crate::protocol::codec::ReadExt;
use crate::protocol::play::Serverbound;
use tuff_wire::PacketReader;

const POSITION: i32 = Id::PlayerPosition.id();
const MOTION: i32 = Id::SetEntityMotion.id();
const KEEP_ALIVE: i32 = Id::KeepAlive.id();
const BATCH_FINISHED: i32 = Id::ChunkBatchFinished.id();

/// The chunk rate a fake claims to manage, per tick.
const CHUNK_RATE: f32 = 25.0;

struct Cursor<'a>(&'a [u8]);

impl<'a> Cursor<'a> {
    fn var_int(&mut self) -> Option<i32> {
        let mut value = 0u32;
        for shift in (0..35).step_by(7) {
            let (&byte, rest) = self.0.split_first()?;
            self.0 = rest;
            value |= u32::from(byte & 0x7F) << shift;
            if byte & 0x80 == 0 {
                return Some(value as i32);
            }
        }
        None
    }

    fn take<const N: usize>(&mut self) -> Option<[u8; N]> {
        let (head, rest) = self.0.split_first_chunk::<N>()?;
        self.0 = rest;
        Some(*head)
    }

    fn double(&mut self) -> Option<f64> {
        self.take().map(f64::from_be_bytes)
    }

    fn float(&mut self) -> Option<f32> {
        self.take().map(f32::from_be_bytes)
    }

    fn long(&mut self) -> Option<i64> {
        self.take().map(i64::from_be_bytes)
    }

    fn lp_vec3(&mut self) -> Option<[f64; 3]> {
        let mut reader = PacketReader::new(Bytes::copy_from_slice(self.0));
        reader.read_lp_vec3().ok()
    }
}

/// Splits `bytes` into the bodies of its uncompressed frames; compressed
/// frames carry large packets, none of which a fake answers.
fn bodies(bytes: &Bytes) -> impl Iterator<Item = &[u8]> {
    let mut rest: &[u8] = bytes;
    std::iter::from_fn(move || {
        while !rest.is_empty() {
            let mut cursor = Cursor(rest);
            let len = usize::try_from(cursor.var_int()?).ok()?;
            let after = cursor.0;
            let frame = after.get(..len)?;
            rest = &after[len..];
            let mut inner = Cursor(frame);
            if inner.var_int()? == 0 {
                return Some(inner.0);
            }
        }
        None
    })
}

enum Reply {
    Teleport {
        id: i32,
        position: [f64; 3],
        velocity: [f64; 3],
        rotation: (f32, f32),
    },
    Impulse([f64; 3]),
    KeepAlive(i64),
    BatchFinished,
}

fn read(body: &[u8], me: i32) -> Option<Reply> {
    let mut c = Cursor(body);
    match c.var_int()? {
        POSITION => {
            let id = c.var_int()?;
            let position = [c.double()?, c.double()?, c.double()?];
            let velocity = [c.double()?, c.double()?, c.double()?];
            let rotation = (c.float()?, c.float()?);
            Some(Reply::Teleport {
                id,
                position,
                velocity,
                rotation,
            })
        }
        MOTION => (c.var_int()? == me)
            .then(|| c.lp_vec3())
            .flatten()
            .map(Reply::Impulse),
        KEEP_ALIVE => Some(Reply::KeepAlive(c.long()?)),
        BATCH_FINISHED => Some(Reply::BatchFinished),
        _ => None,
    }
}

/// Reads one batch of server frames: updates `observed` with what the
/// fake's client learns and returns the packets a client sends back.
pub fn answer(frames: &Bytes, me: i32, observed: &watch::Sender<Observed>) -> Vec<Serverbound> {
    let mut out = Vec::new();
    for reply in bodies(frames).filter_map(|body| read(body, me)) {
        match reply {
            Reply::Teleport {
                id,
                position,
                velocity,
                rotation,
            } => {
                observed.send_modify(|o| {
                    o.position = position;
                    o.velocity = velocity;
                    o.rotation = rotation;
                    o.teleports += 1;
                });
                out.push(Serverbound::AcceptTeleportation {
                    id,
                    position,
                    rotation,
                });
            }
            Reply::Impulse(velocity) => observed.send_modify(|o| {
                o.velocity = velocity;
                o.impulses += 1;
            }),
            Reply::KeepAlive(value) => out.push(Serverbound::KeepAlive(value)),
            Reply::BatchFinished => {
                observed.send_modify(|o| o.chunk_batches += 1);
                out.push(Serverbound::ChunkBatchReceived(CHUNK_RATE));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn var_int(out: &mut Vec<u8>, value: i32) {
        let mut v = value as u32;
        loop {
            let byte = (v & 0x7F) as u8;
            v >>= 7;
            if v == 0 {
                out.push(byte);
                return;
            }
            out.push(byte | 0x80);
        }
    }

    fn frame(id: i32, payload: &[u8]) -> Bytes {
        let mut body = Vec::new();
        var_int(&mut body, 0);
        var_int(&mut body, id);
        body.extend_from_slice(payload);
        let mut out = Vec::new();
        var_int(&mut out, body.len() as i32);
        out.extend(body);
        Bytes::from(out)
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_keep_alive_is_echoed_with_its_value() {
        let (observed, _rx) = watch::channel(Observed::default());
        let out = answer(&frame(KEEP_ALIVE, &77i64.to_be_bytes()), 1, &observed);
        assert!(matches!(out.as_slice(), [Serverbound::KeepAlive(77)]));
    }

    #[tuff_core::watchdog]
    #[test]
    fn each_finished_chunk_batch_is_acked_and_counted() {
        let (observed, rx) = watch::channel(Observed::default());
        let mut frames = frame(BATCH_FINISHED, &[3]).to_vec();
        frames.extend(frame(BATCH_FINISHED, &[1]));
        let out = answer(&Bytes::from(frames), 1, &observed);
        assert_eq!(out.len(), 2);
        assert!(
            out.iter()
                .all(|p| matches!(p, Serverbound::ChunkBatchReceived(r) if *r == CHUNK_RATE))
        );
        assert_eq!(rx.borrow().chunk_batches, 2);
    }

    #[tuff_core::watchdog]
    #[test]
    fn unrelated_frames_are_not_answered() {
        let (observed, rx) = watch::channel(Observed::default());
        let out = answer(&frame(KEEP_ALIVE + 1000, &[0; 8]), 1, &observed);
        assert!(out.is_empty());
        assert_eq!(*rx.borrow(), Observed::default());
    }
}
