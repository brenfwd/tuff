//! Seek behaviour on a long synthetic session: a player walking for twelve
//! minutes with mobs coming and going around it.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use tuff_core::protocol::play::{AddEntity, ForgetLevelChunk, RemoveEntities};
use tuff_replay::script::{Event, Script, Timed};
use tuff_replay::transform::{Transformer, encode};
use tuff_replay::viewer::Viewer;
use tuff_vanilla_data::packet::play::Clientbound as C;
use tuff_wire::PacketWriter;

const TICK_NS: i64 = 50_000_000;
const MINUTES: i64 = 12;
const VIEW: i32 = 8;
const CHUNK_BYTES: usize = 8 * 1024;
const WALK_PER_TICK: f64 = 4.317 / 20.0;
const MOB_LIFE: i64 = 600;

fn raw(kind: C, body: impl FnOnce(&mut PacketWriter)) -> Bytes {
    let mut w = PacketWriter::new();
    w.write_var_int(kind.id());
    body(&mut w);
    w.into_bytes()
}

fn chunk(x: i32, z: i32) -> Bytes {
    raw(C::LevelChunkWithLight, |w| {
        w.write_int(x);
        w.write_int(z);
        w.write_bytes(&[7u8; CHUNK_BYTES]);
    })
}

fn block_update(x: i32, y: i32, z: i32) -> Bytes {
    raw(C::BlockUpdate, |w| {
        w.write_long(
            (i64::from(x) & 0x3FF_FFFF) << 38
                | (i64::from(z) & 0x3FF_FFFF) << 12
                | i64::from(y) & 0xFFF,
        );
        w.write_var_int(1);
    })
}

fn mob(id: i32, x: f64) -> Bytes {
    encode(&AddEntity {
        entity_id: id,
        uuid: id as u128,
        kind: 1,
        position: [x, 64.0, 3.0],
        velocity: [0.0; 3],
        pitch: 0.0,
        yaw: 0.0,
        head_yaw: 0.0,
        data: 0,
    })
}

fn mob_step(id: i32) -> Bytes {
    raw(C::MoveEntityPos, |w| {
        w.write_var_int(id);
        w.write_short(40);
        w.write_short(0);
        w.write_short(0);
        w.write_bool(true);
    })
}

fn session() -> Script {
    let mut events = Vec::new();
    let mut push = |tick: i64, event: Event| {
        events.push(Timed {
            ns: tick * TICK_NS,
            event,
        })
    };
    let mut column = 0;
    for x in -VIEW..=VIEW {
        for z in -VIEW..=VIEW {
            push(0, Event::Packet(chunk(x, z)));
        }
    }
    let mut mobs: Vec<(i32, i64)> = Vec::new();
    let ticks = MINUTES * 60 * 20;
    for tick in 0..=ticks {
        let x = tick as f64 * WALK_PER_TICK;
        push(
            tick,
            Event::Moved {
                position: Some([x, 64.0, 0.0]),
                rotation: Some((-90.0, 0.0)),
            },
        );
        let now = (x as i32) >> 4;
        while column < now {
            for z in -VIEW..=VIEW {
                push(
                    tick,
                    Event::Packet(encode(&ForgetLevelChunk {
                        x: column - VIEW,
                        z,
                    })),
                );
                push(tick, Event::Packet(chunk(column + VIEW + 1, z)));
            }
            column += 1;
        }
        if tick % 20 == 0 {
            push(
                tick,
                Event::Packet(block_update(x as i32 + 3, 64, (tick % 7) as i32)),
            );
        }
        if tick % 10 == 0 {
            let id = tick as i32 / 10 + 1;
            push(tick, Event::Packet(mob(id, x + 8.0)));
            mobs.push((id, tick));
        }
        for &(id, born) in &mobs {
            if (tick - born) % 5 == 0 && tick != born {
                push(tick, Event::Packet(mob_step(id)));
            }
        }
        let (dead, alive): (Vec<_>, Vec<_>) = mobs
            .into_iter()
            .partition(|&(_, born)| tick - born >= MOB_LIFE);
        mobs = alive;
        if !dead.is_empty() {
            let ids: Vec<i32> = dead.iter().map(|&(id, _)| id).collect();
            push(tick, Event::Packet(encode(&RemoveEntities(&ids))));
        }
    }
    Script::from_events("Walker".into(), 1, Vec::new(), events).unwrap()
}

/// Plays the events up to `ns` one by one, syncing after each like a
/// seek without reduction or move batching would.
fn straight(script: &Script, ns: i64) -> (Transformer, usize, usize) {
    let mut transform = Transformer::new(7, script.uuid);
    let mut out = Vec::new();
    for timed in script.events.iter().take_while(|t| t.ns <= ns) {
        transform.apply(&timed.event, &mut out);
        transform.finish(&mut out);
    }
    let bytes = out.iter().map(Bytes::len).sum();
    (transform, out.len(), bytes)
}

fn same_world(a: &Transformer, b: &Transformer) {
    assert_eq!(a.chunks, b.chunks);
    assert_eq!(a.entities, b.entities);
    assert_eq!(a.position, b.position);
    assert_eq!(a.rotation, b.rotation);
}

#[tuff_core::watchdog]
#[test]
fn a_long_seek_sends_only_what_is_left_at_the_target() {
    let script = Arc::new(session());
    let end = script.duration();
    assert!(end >= MINUTES * 60 * 1_000_000_000);
    let (full, full_packets, full_bytes) = straight(&script, end);
    let loaded = full.chunks.len();
    assert_eq!(loaded, ((2 * VIEW + 1) * (2 * VIEW + 1)) as usize);

    let mut viewer = Viewer::new(script.clone(), 7);
    let mut out = Vec::new();
    let started = Instant::now();
    viewer.seek(end, &mut out);
    let forward = started.elapsed();
    let forward_packets = out.len();
    let bytes: usize = out.iter().map(Bytes::len).sum();
    same_world(&viewer.transform, &full);
    assert!(
        bytes < loaded * (CHUNK_BYTES + 64) + 64 * 1024,
        "{bytes} bytes for {loaded} chunks"
    );
    assert!(
        out.len() < full_packets / 100,
        "{} of {full_packets}",
        out.len()
    );

    out.clear();
    let started = Instant::now();
    viewer.seek(end / 2, &mut out);
    let backward = started.elapsed();
    let back_bytes: usize = out.iter().map(Bytes::len).sum();
    let (half, _, half_bytes) = straight(&script, end / 2);
    same_world(&viewer.transform, &half);
    assert!(
        back_bytes < loaded * (CHUNK_BYTES + 64) + 64 * 1024 && back_bytes < half_bytes / 5,
        "{back_bytes} of {half_bytes}"
    );

    eprintln!(
        "{} events over {MINUTES} min; seek to end: {} packets / {bytes} bytes in {forward:?} \
         (unreduced {full_packets} / {full_bytes}); back to half: {} packets / {back_bytes} \
         bytes in {backward:?} (unreduced {half_bytes} bytes)",
        script.events.len(),
        forward_packets,
        out.len(),
    );
}

#[tuff_core::watchdog]
#[test]
fn every_seek_lands_in_the_same_world_as_straight_playback() {
    let script = Arc::new(session());
    let end = script.duration();
    let mut viewer = Viewer::new(script.clone(), 7);
    let mut out = Vec::new();
    for tenth in [9, 2, 5, 5, 0, 7, 3, 10, 1] {
        let at = end / 10 * tenth;
        viewer.seek(at, &mut out);
        same_world(&viewer.transform, &straight(&script, at).0);
        viewer.step(Duration::from_secs(17), &mut out);
        let now = viewer.head.position();
        same_world(&viewer.transform, &straight(&script, now).0);
    }
}
