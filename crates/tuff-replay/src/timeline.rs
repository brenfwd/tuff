//! A block-indexed event timeline spilled to an unlinked-on-drop temp file, so
//! a long recording costs an index in memory and viewers read only the
//! block they are playing.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::Mutex;

use bytes::Bytes;

use crate::script::{Event, Timed};

const BLOCK_EVENTS: usize = 2048;
const BLOCK_BYTES: usize = 1 << 20;

struct Block {
    first_index: usize,
    offset: u64,
    len: usize,
}

struct Spill {
    path: PathBuf,
    file: Mutex<std::fs::File>,
}

impl Drop for Spill {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

pub struct Timeline {
    blocks: Vec<Block>,
    len: usize,
    last_ns: i64,
    spill: Option<Spill>,
}

impl std::fmt::Debug for Timeline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Timeline")
            .field("len", &self.len)
            .field("blocks", &self.blocks.len())
            .finish()
    }
}

impl Timeline {
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Time of the last event, 0 when empty.
    pub fn last_ns(&self) -> i64 {
        self.last_ns
    }

    pub fn block_count(&self) -> usize {
        self.blocks.len()
    }

    /// The events from `index` to the end of its block (empty past the end
    /// or when the spill file cannot be read).
    pub fn read_from(&self, index: usize) -> Vec<Timed> {
        let Some(at) = self
            .blocks
            .partition_point(|b| b.first_index <= index)
            .checked_sub(1)
        else {
            return Vec::new();
        };
        match self.load(at) {
            Ok(mut events) => {
                events.split_off((index - self.blocks[at].first_index).min(events.len()))
            }
            Err(err) => {
                log::warn!("replay timeline block unreadable: {err}");
                Vec::new()
            }
        }
    }

    /// Every event in order, one block at a time.
    pub fn iter(&self) -> impl Iterator<Item = Timed> + '_ {
        let mut next = 0;
        std::iter::from_fn(move || {
            if next >= self.len {
                return None;
            }
            let chunk = self.read_from(next);
            if chunk.is_empty() {
                return None;
            }
            next += chunk.len();
            Some(chunk)
        })
        .flatten()
    }

    fn load(&self, at: usize) -> std::io::Result<Vec<Timed>> {
        let block = &self.blocks[at];
        let spill = self.spill.as_ref().ok_or(std::io::ErrorKind::NotFound)?;
        let mut raw = vec![0u8; block.len];
        {
            let mut file = spill.file.lock().unwrap_or_else(|e| e.into_inner());
            file.seek(SeekFrom::Start(block.offset))?;
            file.read_exact(&mut raw)?;
        }
        let count = self.blocks.get(at + 1).map_or(self.len, |n| n.first_index) - block.first_index;
        decode(&raw, count).ok_or_else(|| std::io::ErrorKind::InvalidData.into())
    }
}

/// Collects events and spills them in blocks.
#[derive(Default)]
pub struct TimelineBuilder {
    pending: Vec<Timed>,
    pending_bytes: usize,
    blocks: Vec<Block>,
    len: usize,
    last_ns: i64,
    spill: Option<Spill>,
    written: u64,
}

impl TimelineBuilder {
    pub fn len(&self) -> usize {
        self.len + self.pending.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn push(&mut self, timed: Timed) -> std::io::Result<()> {
        self.pending_bytes += match &timed.event {
            Event::Packet(p) => p.len(),
            Event::Moved { .. } => 48,
        };
        self.last_ns = timed.ns;
        self.pending.push(timed);
        if self.pending.len() >= BLOCK_EVENTS || self.pending_bytes >= BLOCK_BYTES {
            self.flush()?;
        }
        Ok(())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        if self.spill.is_none() {
            let path = std::env::temp_dir()
                .join(format!("tuff-replay-timeline-{}", rand::random::<u64>()));
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(&path)?;
            self.spill = Some(Spill {
                path,
                file: Mutex::new(file),
            });
        }
        let raw = encode(&self.pending);
        let spill = self.spill.as_ref().expect("created above");
        {
            let mut file = spill.file.lock().unwrap_or_else(|e| e.into_inner());
            file.seek(SeekFrom::Start(self.written))?;
            file.write_all(&raw)?;
        }
        self.blocks.push(Block {
            first_index: self.len,
            offset: self.written,
            len: raw.len(),
        });
        self.written += raw.len() as u64;
        self.len += self.pending.len();
        self.pending.clear();
        self.pending_bytes = 0;
        Ok(())
    }

    pub fn finish(mut self) -> std::io::Result<Timeline> {
        self.flush()?;
        Ok(Timeline {
            blocks: self.blocks,
            len: self.len,
            last_ns: self.last_ns,
            spill: self.spill,
        })
    }
}

fn encode(events: &[Timed]) -> Vec<u8> {
    let mut out = Vec::new();
    for t in events {
        out.extend_from_slice(&t.ns.to_le_bytes());
        match &t.event {
            Event::Packet(p) => {
                out.push(0);
                out.extend_from_slice(&(p.len() as u32).to_le_bytes());
                out.extend_from_slice(p);
            }
            Event::Moved { position, rotation } => {
                out.push(1 | (position.is_some() as u8) << 1 | (rotation.is_some() as u8) << 2);
                for v in position.unwrap_or_default() {
                    out.extend_from_slice(&v.to_le_bytes());
                }
                let (y, p) = rotation.unwrap_or_default();
                out.extend_from_slice(&y.to_le_bytes());
                out.extend_from_slice(&p.to_le_bytes());
            }
        }
    }
    out
}

fn decode(raw: &[u8], count: usize) -> Option<Vec<Timed>> {
    let mut at = 0usize;
    let mut take = |n: usize| -> Option<&[u8]> {
        let s = raw.get(at..at.checked_add(n)?)?;
        at += n;
        Some(s)
    };
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let ns = i64::from_le_bytes(take(8)?.try_into().ok()?);
        let kind = take(1)?[0];
        let event = if kind == 0 {
            let n = u32::from_le_bytes(take(4)?.try_into().ok()?) as usize;
            Event::Packet(Bytes::copy_from_slice(take(n)?))
        } else {
            let mut pos = [0f64; 3];
            for v in &mut pos {
                *v = f64::from_le_bytes(take(8)?.try_into().ok()?);
            }
            let yaw = f32::from_le_bytes(take(4)?.try_into().ok()?);
            let pitch = f32::from_le_bytes(take(4)?.try_into().ok()?);
            Event::Moved {
                position: (kind & 2 != 0).then_some(pos),
                rotation: (kind & 4 != 0).then_some((yaw, pitch)),
            }
        };
        out.push(Timed { ns, event });
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build(n: usize) -> Timeline {
        let mut b = TimelineBuilder::default();
        for i in 0..n {
            let event = if i % 3 == 0 {
                Event::Moved {
                    position: Some([i as f64, 1.0, 2.0]),
                    rotation: (i % 2 == 0).then_some((1.5, -2.0)),
                }
            } else {
                Event::Packet(Bytes::from(vec![i as u8; 5]))
            };
            b.push(Timed {
                ns: i as i64 * 10,
                event,
            })
            .unwrap();
        }
        b.finish().unwrap()
    }

    #[tuff_core::watchdog]
    #[test]
    fn events_round_trip_across_blocks_and_read_from_any_index() {
        let n = BLOCK_EVENTS * 2 + 17;
        let t = build(n);
        assert_eq!(t.len(), n);
        assert_eq!(t.block_count(), 3);
        assert_eq!(t.last_ns(), (n as i64 - 1) * 10);
        assert_eq!(t.iter().count(), n);
        let from = t.read_from(BLOCK_EVENTS + 5);
        assert_eq!(from.len(), BLOCK_EVENTS - 5);
        assert_eq!(from[0].ns, (BLOCK_EVENTS as i64 + 5) * 10);
        assert!(t.read_from(n).is_empty());
        assert!(matches!(t.read_from(3)[0].event, Event::Moved { .. }));
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_spill_file_is_removed_on_drop() {
        let t = build(10);
        let path = t.spill.as_ref().unwrap().path.clone();
        assert!(path.exists());
        drop(t);
        assert!(!path.exists());
    }
}
