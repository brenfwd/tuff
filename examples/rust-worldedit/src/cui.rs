// SPDX-License-Identifier: MIT
use crate::region::P;
use crate::selection::{Kind, Sel};
use crate::session::{Player, model};

pub const CHANNEL: &str = "worldedit:cui";

pub fn accepts_handshake(msg: &str) -> Option<u32> {
    msg.strip_prefix("v|")?.parse().ok()
}

fn volume_of(sel: &Sel) -> i64 {
    match (sel.kind, sel.a, sel.b) {
        (Kind::Cuboid | Kind::Extend, Some(a), Some(b)) => {
            (0..3).map(|i| i64::from((a[i] - b[i]).abs()) + 1).product()
        }
        _ => -1,
    }
}

pub fn plan(player: &mut Player) -> Vec<String> {
    if !player.cui || !player.draw {
        return Vec::new();
    }
    let current = messages(&player.sel);
    if current == player.cui_sent {
        return Vec::new();
    }
    player.cui_sent = current.clone();
    current
}

fn transmit(id: u128, lines: &[String]) {
    let uuid = format!("{id:032x}");
    for line in lines {
        let _ = tuff_plugin_sdk::events::send_payload(&uuid, CHANNEL, line.as_bytes());
    }
}

pub fn sync(id: u128) {
    let lines = model(|m| m.players.get_mut(&id).map(plan).unwrap_or_default());
    transmit(id, &lines);
}

pub fn handshake(id: u128, version: u32) {
    model(|m| {
        if let Some(p) = m.players.get_mut(&id) {
            p.cui = version > 0;
            p.cui_sent.clear();
        }
    });
    sync(id);
}

pub fn resend(id: u128) {
    model(|m| {
        if let Some(p) = m.players.get_mut(&id) {
            p.cui_sent.clear();
        }
    });
    sync(id);
}

pub fn set_draw(id: u128, on: bool) {
    let cleared = model(|m| {
        let p = m.players.get_mut(&id)?;
        p.draw = on;
        p.cui_sent.clear();
        (!on && p.cui).then(|| messages(&Sel::default()))
    });
    match cleared {
        Some(lines) => transmit(id, &lines),
        None => sync(id),
    }
}

pub fn on_payload(message: &tuff_plugin_sdk::types::PayloadMessage) {
    if message.channel != CHANNEL {
        return;
    }
    let Some(version) = std::str::from_utf8(&message.data)
        .ok()
        .and_then(accepts_handshake)
    else {
        return;
    };
    if let Some(id) = crate::session::owner() {
        handshake(id, version);
    }
}

pub fn messages(sel: &Sel) -> Vec<String> {
    let mut out = Vec::new();
    match sel.kind {
        Kind::Cuboid | Kind::Extend => {
            out.push("s|cuboid".to_string());
            let vol = volume_of(sel);
            for (i, p) in [sel.a, sel.b].into_iter().enumerate() {
                if let Some(p) = p {
                    out.push(format!("p|{i}|{}|{}|{}|{vol}", p[0], p[1], p[2]));
                }
            }
        }
        Kind::Poly => {
            out.push("s|poly2d".to_string());
            for (i, p) in sel.pts.iter().enumerate() {
                out.push(format!("p|{i}|{}|{}|-1", p[0], p[2]));
            }
            out.push(format!("mm|{}|{}", sel.y[0], sel.y[1]));
        }
        Kind::Ellipsoid | Kind::Sphere => {
            out.push("s|ellipsoid".to_string());
            if let Some(c) = sel.a {
                out.push(format!("e|0|{}|{}|{}", c[0], c[1], c[2]));
                let r: P = if sel.kind == Kind::Sphere {
                    [sel.radius[0]; 3]
                } else {
                    sel.radius
                };
                out.push(format!("e|1|{}|{}|{}", r[0], r[1], r[2]));
            }
        }
        Kind::Cyl => {
            out.push("s|cylinder".to_string());
            if let Some(c) = sel.a {
                out.push(format!(
                    "cyl|{}|{}|{}|{}|{}",
                    c[0], c[1], c[2], sel.radius[0], sel.radius[2]
                ));
                out.push(format!("mm|{}|{}", sel.y[0], sel.y[1]));
            }
        }
        Kind::Convex => {
            out.push("s|polyhedron".to_string());
            for (i, p) in sel.pts.iter().enumerate() {
                out.push(format!("p|{i}|{}|{}|{}|-1", p[0], p[1], p[2]));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_sends_only_changes_to_handshaken_players() {
        let mut p = Player::default();
        assert!(plan(&mut p).is_empty());
        p.cui = true;
        assert_eq!(plan(&mut p), ["s|cuboid"]);
        assert!(plan(&mut p).is_empty());
        p.sel.a = Some([1, 2, 3]);
        assert_eq!(plan(&mut p), ["s|cuboid", "p|0|1|2|3|-1"]);
        p.draw = false;
        p.sel.b = Some([1, 2, 3]);
        assert!(plan(&mut p).is_empty());
    }

    #[test]
    fn handshake() {
        assert_eq!(accepts_handshake("v|4"), Some(4));
        assert_eq!(accepts_handshake("x|4"), None);
    }

    #[test]
    fn cuboid() {
        let sel = Sel {
            a: Some([0, 0, 0]),
            b: Some([1, 2, 3]),
            ..Sel::default()
        };
        assert_eq!(messages(&sel), ["s|cuboid", "p|0|0|0|0|24", "p|1|1|2|3|24"]);
    }

    #[test]
    fn poly_and_convex() {
        let poly = Sel {
            kind: Kind::Poly,
            pts: vec![[1, 0, 2]],
            y: [3, 9],
            ..Sel::default()
        };
        assert_eq!(messages(&poly), ["s|poly2d", "p|0|1|2|-1", "mm|3|9"]);
        let hull = Sel {
            kind: Kind::Convex,
            pts: vec![[1, 2, 3]],
            ..Sel::default()
        };
        assert_eq!(messages(&hull), ["s|polyhedron", "p|0|1|2|3|-1"]);
    }

    #[test]
    fn round_shapes() {
        let e = Sel {
            kind: Kind::Ellipsoid,
            a: Some([1, 2, 3]),
            radius: [4, 5, 6],
            ..Sel::default()
        };
        assert_eq!(messages(&e), ["s|ellipsoid", "e|0|1|2|3", "e|1|4|5|6"]);
        let c = Sel {
            kind: Kind::Cyl,
            a: Some([1, 2, 3]),
            radius: [4, 0, 5],
            y: [0, 7],
            ..Sel::default()
        };
        assert_eq!(messages(&c), ["s|cylinder", "cyl|1|2|3|4|5", "mm|0|7"]);
    }
}
