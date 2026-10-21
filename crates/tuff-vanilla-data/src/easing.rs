use std::f64::consts::PI;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    In,
    Out,
    InOut,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Curve {
    Back,
    Bounce,
    Circ,
    Cubic,
    Elastic,
    Expo,
    Quad,
    Quart,
    Quint,
    Sine,
}

/// One of the named easings a timeline track selects with `ease`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Easing {
    pub direction: Direction,
    pub curve: Curve,
}

const CURVES: [(&str, Curve); 10] = [
    ("back", Curve::Back),
    ("bounce", Curve::Bounce),
    ("circ", Curve::Circ),
    ("cubic", Curve::Cubic),
    ("elastic", Curve::Elastic),
    ("expo", Curve::Expo),
    ("quad", Curve::Quad),
    ("quart", Curve::Quart),
    ("quint", Curve::Quint),
    ("sine", Curve::Sine),
];

fn out_bounce(x: f64) -> f64 {
    if x < 0.363_636_37 {
        7.5625 * x * x
    } else if x < 0.727_272_75 {
        7.5625 * (x - 0.545_454_56).powi(2) + 0.75
    } else if x < 0.909_090_909_090_909_1 {
        7.5625 * (x - 0.818_181_8).powi(2) + 0.9375
    } else {
        7.5625 * (x - 0.954_545_44).powi(2) + 0.984_375
    }
}

impl Easing {
    pub const fn new(direction: Direction, curve: Curve) -> Self {
        Self { direction, curve }
    }

    /// The easing named `in_back`, `out_sine`, `in_out_quad` and so on.
    pub fn from_name(name: &str) -> Option<Self> {
        let (direction, rest) = if let Some(rest) = name.strip_prefix("in_out_") {
            (Direction::InOut, rest)
        } else if let Some(rest) = name.strip_prefix("in_") {
            (Direction::In, rest)
        } else {
            (Direction::Out, name.strip_prefix("out_")?)
        };
        CURVES
            .iter()
            .find(|(curve, _)| *curve == rest)
            .map(|(_, curve)| Self::new(direction, *curve))
    }

    /// The eased progress for `x` in `0..=1`.
    pub fn apply(self, x: f32) -> f32 {
        let x = f64::from(x);
        let y = match self.direction {
            Direction::In => self.ease_in(x),
            Direction::Out => self.ease_out(x),
            Direction::InOut => self.ease_in_out(x),
        };
        y as f32
    }

    fn ease_in(self, x: f64) -> f64 {
        match self.curve {
            Curve::Back => x * x * (2.70158 * x - 1.70158),
            Curve::Bounce => 1.0 - out_bounce(1.0 - x),
            Curve::Circ => 1.0 - (1.0 - x * x).sqrt(),
            Curve::Cubic => x * x * x,
            Curve::Elastic if x == 0.0 || x == 1.0 => x,
            Curve::Elastic => {
                -(2f64.powf(10.0 * x - 10.0)) * ((x * 10.0 - 10.75) * (2.0 * PI / 3.0)).sin()
            }
            Curve::Expo if x == 0.0 => 0.0,
            Curve::Expo => 2f64.powf(10.0 * x - 10.0),
            Curve::Quad => x * x,
            Curve::Quart => x.powi(4),
            Curve::Quint => x.powi(5),
            Curve::Sine => 1.0 - (x * PI / 2.0).cos(),
        }
    }

    fn ease_out(self, x: f64) -> f64 {
        match self.curve {
            Curve::Back => 1.0 + 2.70158 * (x - 1.0).powi(3) + 1.70158 * (x - 1.0).powi(2),
            Curve::Bounce => out_bounce(x),
            Curve::Circ => (1.0 - (x - 1.0).powi(2)).sqrt(),
            Curve::Cubic => 1.0 - (1.0 - x).powi(3),
            Curve::Elastic if x == 0.0 || x == 1.0 => x,
            Curve::Elastic => {
                2f64.powf(-10.0 * x) * ((x * 10.0 - 0.75) * (2.0 * PI / 3.0)).sin() + 1.0
            }
            Curve::Expo if x == 1.0 => 1.0,
            Curve::Expo => 1.0 - 2f64.powf(-10.0 * x),
            Curve::Quad => 1.0 - (1.0 - x).powi(2),
            Curve::Quart => 1.0 - (1.0 - x).powi(4),
            Curve::Quint => 1.0 - (1.0 - x).powi(5),
            Curve::Sine => (x * PI / 2.0).sin(),
        }
    }

    fn ease_in_out(self, x: f64) -> f64 {
        let first = x < 0.5;
        match self.curve {
            Curve::Back if first => 4.0 * x * x * (7.189819 * x - 2.5949094) / 2.0,
            Curve::Back => {
                let dt = 2.0 * x - 2.0;
                (dt * dt * (3.5949094 * dt + 2.5949094) + 2.0) / 2.0
            }
            Curve::Bounce if first => (1.0 - out_bounce(1.0 - 2.0 * x)) / 2.0,
            Curve::Bounce => (1.0 + out_bounce(2.0 * x - 1.0)) / 2.0,
            Curve::Circ if first => (1.0 - (1.0 - (2.0 * x).powi(2)).sqrt()) / 2.0,
            Curve::Circ => ((1.0 - (-2.0 * x + 2.0).powi(2)).sqrt() + 1.0) / 2.0,
            Curve::Cubic if first => 4.0 * x * x * x,
            Curve::Cubic => 1.0 - (-2.0 * x + 2.0).powi(3) / 2.0,
            Curve::Elastic if x == 0.0 || x == 1.0 => x,
            Curve::Elastic => {
                let sin = ((20.0 * x - 11.125) * (2.0 * PI / 4.5)).sin();
                if first {
                    -(2f64.powf(20.0 * x - 10.0) * sin) / 2.0
                } else {
                    2f64.powf(-20.0 * x + 10.0) * sin / 2.0 + 1.0
                }
            }
            Curve::Expo if x == 0.0 || x == 1.0 => x,
            Curve::Expo if first => 2f64.powf(20.0 * x - 10.0) / 2.0,
            Curve::Expo => (2.0 - 2f64.powf(-20.0 * x + 10.0)) / 2.0,
            Curve::Quad if first => 2.0 * x * x,
            Curve::Quad => 1.0 - (-2.0 * x + 2.0).powi(2) / 2.0,
            Curve::Quart if first => 8.0 * x.powi(4),
            Curve::Quart => 1.0 - (-2.0 * x + 2.0).powi(4) / 2.0,
            Curve::Quint if first => 16.0 * x.powi(5),
            Curve::Quint => 1.0 - (-2.0 * x + 2.0).powi(5) / 2.0,
            Curve::Sine => -((PI * x).cos() - 1.0) / 2.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NAMES: [&str; 30] = [
        "in_back",
        "in_bounce",
        "in_circ",
        "in_cubic",
        "in_elastic",
        "in_expo",
        "in_quad",
        "in_quart",
        "in_quint",
        "in_sine",
        "out_back",
        "out_bounce",
        "out_circ",
        "out_cubic",
        "out_elastic",
        "out_expo",
        "out_quad",
        "out_quart",
        "out_quint",
        "out_sine",
        "in_out_back",
        "in_out_bounce",
        "in_out_circ",
        "in_out_cubic",
        "in_out_elastic",
        "in_out_expo",
        "in_out_quad",
        "in_out_quart",
        "in_out_quint",
        "in_out_sine",
    ];

    #[test]
    fn every_named_easing_starts_at_zero_and_ends_at_one() {
        for name in NAMES {
            let easing = Easing::from_name(name).unwrap_or_else(|| panic!("{name}"));
            assert!(easing.apply(0.0).abs() < 1e-5, "{name} start");
            assert!((easing.apply(1.0) - 1.0).abs() < 1e-5, "{name} end");
        }
        assert_eq!(Easing::from_name("linear"), None);
        assert_eq!(Easing::from_name("in_wobble"), None);
    }

    #[test]
    fn easings_match_their_curves_at_the_midpoint() {
        let at = |name: &str, x: f32| Easing::from_name(name).unwrap().apply(x);
        assert!((at("in_quad", 0.5) - 0.25).abs() < 1e-6);
        assert!((at("out_quad", 0.5) - 0.75).abs() < 1e-6);
        assert!((at("in_out_cubic", 0.25) - 0.0625).abs() < 1e-6);
        assert!((at("in_out_sine", 0.5) - 0.5).abs() < 1e-6);
        assert!(at("in_back", 0.2) < 0.0);
    }
}
