// SPDX-License-Identifier: MIT
use crate::edit::{Entry, Job, Snap};
use crate::jobs::{busy, start};
use crate::session::{model, say};

const MAX_HISTORY: usize = 10;
const STORED_CAP: u64 = 8_000_000;
const SAVED_BYTES: usize = 512 * 1024;
const VERSION: u8 = 2;

pub fn history_many(id: u128, redo: bool, times: u32) -> i32 {
    model(|m| m.players.get_mut(&id).unwrap().pending = Some((redo, times - 1)));
    let said = step(id, redo, true);
    if model(|m| m.jobs.iter().all(|j| j.owner != id)) {
        model(|m| m.players.get_mut(&id).unwrap().pending = None);
    }
    said
}

pub fn clear(id: u128) -> i32 {
    model(|m| {
        let p = m.players.get_mut(&id).unwrap();
        p.undo.clear();
        p.redo.clear();
    });
    save(id);
    say("History cleared")
}

fn step(id: u128, redo: bool, loud: bool) -> i32 {
    if let Some(why) = busy(id) {
        return say(why);
    }
    let entry = model(|m| {
        let p = m.players.get_mut(&id).unwrap();
        match redo {
            true => p.redo.pop_back(),
            false => p.undo.pop_back(),
        }
    });
    let Some(entry) = entry else {
        model(|m| m.players.get_mut(&id).unwrap().pending = None);
        if !loud {
            return 0;
        }
        return say(match redo {
            true => "Nothing to redo",
            false => "Nothing to undo",
        });
    };
    start(Job::restore(id, entry, redo))
}

pub fn commit(job: Job) {
    let owner = job.owner;
    record(job);
    let next = model(|m| {
        let p = m.players.get_mut(&owner)?;
        match p.pending.take() {
            Some((redo, left)) if left > 0 => {
                p.pending = Some((redo, left - 1));
                Some(redo)
            }
            _ => None,
        }
    });
    if let Some(redo) = next {
        step(owner, redo, false);
    }
}

fn record(job: Job) {
    let owner = job.owner;
    if job.restoring.is_none() && model(|m| m.players.get(&owner).is_some_and(|p| p.fast)) {
        return;
    }
    let restoring = job.restoring;
    let entry = job.into_entry();
    if entry.blocks == 0 {
        return;
    }
    model(|m| {
        let mut total: u64 = m
            .players
            .values()
            .flat_map(|p| p.undo.iter().chain(p.redo.iter()))
            .map(|e| e.blocks)
            .sum::<u64>()
            + entry.blocks;
        let Some(p) = m.players.get_mut(&owner) else {
            return;
        };
        if restoring.is_none() {
            p.redo.clear();
        }
        let target = if restoring == Some(false) {
            &mut p.redo
        } else {
            &mut p.undo
        };
        target.push_back(entry);
        while target.len() > MAX_HISTORY || (total > STORED_CAP && target.len() > 1) {
            if let Some(old) = target.pop_front() {
                total = total.saturating_sub(old.blocks);
            }
        }
        if total > STORED_CAP {
            target.pop_back();
        }
    });
    save(owner);
}

fn key(id: u128) -> String {
    format!("history/{id:032x}")
}

fn put_str(out: &mut Vec<u8>, s: &str) {
    out.extend((s.len() as u32).to_le_bytes());
    out.extend(s.as_bytes());
}

fn encode_entry(e: &Entry) -> Vec<u8> {
    let mut out = Vec::new();
    put_str(&mut out, &e.dim);
    out.extend(e.blocks.to_le_bytes());
    out.extend((e.snaps.len() as u32).to_le_bytes());
    for s in &e.snaps {
        for v in s.min.iter().chain(&s.size) {
            out.extend(v.to_le_bytes());
        }
        out.push(u8::from(s.biome));
        out.extend((s.palette.len() as u32).to_le_bytes());
        for p in &s.palette {
            put_str(&mut out, p);
        }
        out.extend((s.idx.len() as u32).to_le_bytes());
        for i in &s.idx {
            out.extend(i.to_le_bytes());
        }
    }
    out
}

struct Reader<'a>(&'a [u8]);

impl Reader<'_> {
    fn take(&mut self, n: usize) -> Option<&[u8]> {
        if self.0.len() < n {
            return None;
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Some(a)
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    fn i32(&mut self) -> Option<i32> {
        Some(self.u32()? as i32)
    }
    fn str(&mut self) -> Option<String> {
        let n = self.u32()? as usize;
        String::from_utf8(self.take(n)?.to_vec()).ok()
    }
}

fn decode_entry(r: &mut Reader) -> Option<Entry> {
    let dim = r.str()?;
    let blocks = u64::from_le_bytes(r.take(8)?.try_into().ok()?);
    let n = r.u32()?;
    let mut snaps = Vec::new();
    for _ in 0..n {
        let mut v = [0i32; 6];
        for x in &mut v {
            *x = r.i32()?;
        }
        let biome = r.take(1)?[0] != 0;
        let pn = r.u32()?;
        let mut palette = Vec::new();
        for _ in 0..pn {
            palette.push(r.str()?);
        }
        let n = r.u32()? as usize;
        let raw = r.take(n.checked_mul(2)?)?;
        let idx = raw
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .collect();
        snaps.push(Snap {
            min: [v[0], v[1], v[2]],
            size: [v[3], v[4], v[5]],
            palette,
            idx,
            biome,
        });
    }
    Some(Entry { dim, snaps, blocks })
}

fn encode(undo: &[Vec<u8>], redo: &[Vec<u8>]) -> Vec<u8> {
    let mut out = vec![VERSION];
    for list in [undo, redo] {
        out.extend((list.len() as u32).to_le_bytes());
        for e in list {
            out.extend(e);
        }
    }
    out
}

fn decode(bytes: &[u8]) -> Option<(Vec<Entry>, Vec<Entry>)> {
    let mut r = Reader(bytes);
    if r.take(1)? != [VERSION] {
        return None;
    }
    let mut lists = [Vec::new(), Vec::new()];
    for list in &mut lists {
        let n = r.u32()?;
        for _ in 0..n {
            list.push(decode_entry(&mut r)?);
        }
    }
    let [a, b] = lists;
    Some((a, b))
}

fn save(id: u128) {
    let (mut undo, mut redo) = model(|m| {
        let Some(p) = m.players.get(&id) else {
            return (Vec::new(), Vec::new());
        };
        (
            p.undo.iter().map(encode_entry).collect::<Vec<_>>(),
            p.redo.iter().map(encode_entry).collect::<Vec<_>>(),
        )
    });
    loop {
        let size: usize = undo.iter().chain(&redo).map(Vec::len).sum::<usize>() + 9;
        if size <= SAVED_BYTES {
            break;
        }
        if !undo.is_empty() {
            undo.remove(0);
        } else if !redo.is_empty() {
            redo.remove(0);
        }
    }
    let k = key(id);
    if undo.is_empty() && redo.is_empty() {
        let _ = tuff_plugin_sdk::store::remove_bytes(&k);
    } else {
        let _ = tuff_plugin_sdk::store::set_bytes(&k, &encode(&undo, &redo));
    }
}

pub fn load(id: u128) {
    let Some((undo, redo)) = tuff_plugin_sdk::store::get_bytes(&key(id)).and_then(|b| decode(&b))
    else {
        return;
    };
    model(|m| {
        if let Some(p) = m.players.get_mut(&id) {
            p.undo = undo.into();
            p.redo = redo.into();
        }
    });
}
