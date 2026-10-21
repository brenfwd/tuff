//! The clamp every ridden vehicle shares: the driving client reports the
//! position, the server runs the vehicle itself as a shadow, and a report is
//! held within `SHADOW_RANGE` of that shadow.

/// Blocks a driver's client may be ahead of or behind the shadow.
pub const SHADOW_RANGE: f64 = 2.0;
/// The share of the gap to the client's position the shadow closes on each
/// accepted move, so honest drift fades while a faster client stays ahead.
pub const SHADOW_PULL: f64 = 0.03;

pub fn distance(a: [f64; 3], b: [f64; 3]) -> f64 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

/// Holds `reported` within range of `shadow` and pulls the shadow toward
/// what it held. Returns the held position.
pub fn hold(shadow: &mut [f64; 3], reported: [f64; 3]) -> [f64; 3] {
    let gap = [0, 1, 2].map(|i| reported[i] - shadow[i]);
    let length = distance(gap, [0.0; 3]);
    let scale = if length > SHADOW_RANGE {
        SHADOW_RANGE / length
    } else {
        1.0
    };
    let held = [0, 1, 2].map(|i| shadow[i] + gap[i] * scale);
    for i in 0..3 {
        shadow[i] += gap[i] * scale * SHADOW_PULL;
    }
    held
}
