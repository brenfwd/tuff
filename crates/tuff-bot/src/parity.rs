use tuff_core::protocol::{Encode, play};

pub const CAPTURE_IDS: [i32; 2] = [play::LevelChunkWithLight::ID, play::BlockChangedAck::ID];

pub fn normalize_kept(kept: &[(i32, bytes::Bytes)]) -> Vec<String> {
    kept.iter()
        .filter_map(|(id, body)| kept_event(*id, body))
        .collect()
}

fn kept_event(id: i32, body: &[u8]) -> Option<String> {
    if id == play::LevelChunkWithLight::ID {
        let x = i32::from_be_bytes(body.get(0..4)?.try_into().ok()?);
        let z = i32::from_be_bytes(body.get(4..8)?.try_into().ok()?);
        Some(format!("chunk_send {x} {z}"))
    } else if id == play::BlockChangedAck::ID {
        let mut seq = 0u32;
        for (i, b) in body.iter().take(5).enumerate() {
            seq |= u32::from(b & 0x7f) << (7 * i);
            if b & 0x80 == 0 {
                return Some(format!("block_ack {}", seq as i32));
            }
        }
        None
    } else {
        None
    }
}

/// Reduces a vanilla `events.txt` (from `scripts/vanilla-parity-run.sh`) to the
/// same normalized lines tuff's client-side capture produces.
pub fn parse_vanilla(text: &str) -> Vec<String> {
    text.lines().filter_map(vanilla_event).collect()
}

/// Chunk tickets are server-internal (not visible to a bot), so they are
/// extracted separately for vanilla-only inspection.
pub fn vanilla_tickets(text: &str) -> Vec<String> {
    text.lines().filter_map(vanilla_ticket).collect()
}

/// Vanilla's forceloaded chunks as sorted, deduplicated `ticket x z` lines.
/// Only ticket lines naming a forced type count; other ticket kinds (player
/// loading, portals) have no tuff-side capture.
pub fn vanilla_forced_tickets(text: &str) -> Vec<String> {
    let forced: Vec<&str> = text
        .lines()
        .filter(|l| l.to_ascii_lowercase().contains("forced"))
        .collect();
    let mut out: Vec<String> = forced.into_iter().filter_map(vanilla_ticket).collect();
    out.sort();
    out.dedup();
    out
}

/// Tuff's forceloaded chunks as sorted, deduplicated `ticket x z` lines.
pub fn normalize_tickets(chunks: &[(i32, i32)]) -> Vec<String> {
    let mut out: Vec<String> = chunks
        .iter()
        .map(|(x, z)| format!("ticket {x} {z}"))
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Set difference of two ticket line lists; order is not a contract.
pub fn diff_tickets(vanilla: &[String], tuff: &[String]) -> Vec<String> {
    let only = |a: &[String], b: &[String], who: &str| {
        a.iter()
            .filter(|l| !b.contains(l))
            .map(|l| format!("{l}: only in {who}"))
            .collect::<Vec<_>>()
    };
    let mut out = only(vanilla, tuff, "vanilla");
    out.extend(only(tuff, vanilla, "tuff"));
    out.sort();
    out
}

fn vanilla_event(line: &str) -> Option<String> {
    let msg = line.split_once("]: ").map_or(line, |(_, m)| m);
    let msg = msg
        .strip_prefix("DEBUG ")
        .or_else(|| msg.strip_prefix("INFO "))
        .unwrap_or(msg);
    if let Some(rest) = msg.strip_prefix("SEN [") {
        let (x, z) = rest.trim_end().trim_end_matches(']').split_once(", ")?;
        return Some(format!("chunk_send {} {}", x.trim(), z.trim()));
    }
    let rest = msg.strip_prefix("Server ACK ")?;
    let seq: i32 = rest.split_whitespace().next()?.parse().ok()?;
    Some(format!("block_ack {seq}"))
}

fn vanilla_ticket(line: &str) -> Option<String> {
    let msg = line.split_once("]: ").map_or(line, |(_, m)| m);
    let msg = msg.strip_prefix("DEBUG ").unwrap_or(msg);
    let rest = msg.strip_prefix("ATI [")?;
    let (x, rest) = rest.split_once(", ")?;
    let (z, _) = rest.split_once(']')?;
    Some(format!("ticket {} {}", x.trim(), z.trim()))
}

/// Compares two normalized sequences. Chunk sends are order-insensitive within
/// a run of consecutive sends (batch order is not a contract); everything else
/// must match in order. Returns human-readable mismatches.
pub fn diff(vanilla: &[String], tuff: &[String]) -> Vec<String> {
    let (a, b) = (canonical(vanilla), canonical(tuff));
    let mut out = Vec::new();
    for i in 0..a.len().max(b.len()) {
        let (l, r) = (a.get(i), b.get(i));
        if l != r {
            out.push(format!(
                "#{i}: vanilla={} tuff={}",
                l.map_or("<none>", String::as_str),
                r.map_or("<none>", String::as_str)
            ));
        }
    }
    out
}

fn canonical(events: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(events.len());
    let mut run = Vec::new();
    for e in events {
        if e.starts_with("chunk_send ") {
            run.push(e.clone());
        } else {
            run.sort();
            out.append(&mut run);
            out.push(e.clone());
        }
    }
    run.sort();
    out.append(&mut run);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn kept_packets_normalize_to_events() {
        let mut chunk = Vec::new();
        chunk.extend_from_slice(&(-3i32).to_be_bytes());
        chunk.extend_from_slice(&7i32.to_be_bytes());
        chunk.extend_from_slice(&[1, 2, 3]);
        let kept = vec![
            (play::LevelChunkWithLight::ID, bytes::Bytes::from(chunk)),
            (
                play::BlockChangedAck::ID,
                bytes::Bytes::from_static(&[0x81, 0x01]),
            ),
            (999, bytes::Bytes::new()),
        ];
        assert_eq!(normalize_kept(&kept), ["chunk_send -3 7", "block_ack 129"]);
    }

    #[tuff_core::watchdog]
    #[test]
    fn vanilla_log_lines_normalize() {
        let log = "DEBUG SEN [4, -2]\nINFO Server ACK 5 BlockPos{x=1, y=2, z=3} true accept destroy\nINFO other\nDEBUG ATI [0, 1] Ticket[x]";
        assert_eq!(parse_vanilla(log), ["chunk_send 4 -2", "block_ack 5"]);
        assert_eq!(vanilla_tickets(log), ["ticket 0 1"]);
    }

    #[tuff_core::watchdog]
    #[test]
    fn diff_ignores_send_order_within_a_run_only() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let a = s(&["chunk_send 0 0", "chunk_send 1 0", "block_ack 1"]);
        let b = s(&["chunk_send 1 0", "chunk_send 0 0", "block_ack 1"]);
        assert!(diff(&a, &b).is_empty());
        let c = s(&["block_ack 1", "chunk_send 0 0", "chunk_send 1 0"]);
        assert_eq!(diff(&a, &c).len(), 3);
    }

    #[tuff_core::watchdog]
    #[test]
    fn tickets_normalize_sorted_and_deduped() {
        assert_eq!(
            normalize_tickets(&[(2, -1), (0, 0), (2, -1)]),
            ["ticket 0 0", "ticket 2 -1"]
        );
        let log = "DEBUG ATI [2, -1] Ticket[forced]\nDEBUG ATI [2, -1] Ticket[forced]\nDEBUG ATI [5, 5] Ticket[player]\nDEBUG ATI [0, 0] Ticket[minecraft:forced]";
        assert_eq!(vanilla_forced_tickets(log), ["ticket 0 0", "ticket 2 -1"]);
    }

    #[tuff_core::watchdog]
    #[test]
    fn ticket_diff_is_order_insensitive_and_names_the_side() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let a = s(&["ticket 0 0", "ticket 1 1"]);
        assert!(diff_tickets(&a, &s(&["ticket 1 1", "ticket 0 0"])).is_empty());
        assert_eq!(
            diff_tickets(&a, &s(&["ticket 1 1", "ticket 3 3"])),
            ["ticket 0 0: only in vanilla", "ticket 3 3: only in tuff"]
        );
    }
}
