//! Positions and rotations as commands write them: absolute numbers,
//! `~` offsets from the source, and `^` offsets along the source's view.

use super::error::CommandError;
use super::reader::Reader;

/// Where a command runs from: what `~` and `^` are relative to.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pose {
    pub position: [f64; 3],
    /// Yaw and pitch in degrees.
    pub rotation: [f32; 2],
    /// The `^` origin: the anchor applied to the position.
    pub anchor: [f64; 3],
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Coordinate {
    pub relative: bool,
    pub value: f64,
}

impl Coordinate {
    pub fn get(&self, original: f64) -> f64 {
        if self.relative {
            self.value + original
        } else {
            self.value
        }
    }

    fn is_relative(reader: &mut Reader<'_>) -> bool {
        if reader.peek_is('~') {
            reader.skip();
            true
        } else {
            false
        }
    }

    /// A number with an optional `~`; `center` adds 0.5 to a bare integer.
    pub fn parse_double(reader: &mut Reader<'_>, center: bool) -> Result<Self, CommandError> {
        if reader.peek_is('^') {
            return Err(CommandError::simple("argument.pos.mixed").at(reader));
        }
        if !reader.can_read() {
            return Err(CommandError::simple("argument.pos.missing.double").at(reader));
        }
        let relative = Self::is_relative(reader);
        let start = reader.cursor();
        let mut value = if !reader.can_read() || reader.peek_is(' ') {
            0.0
        } else {
            reader.read_double()?
        };
        let number = &reader.input()[start..reader.cursor()];
        if relative && number.is_empty() {
            return Ok(Self {
                relative: true,
                value: 0.0,
            });
        }
        if !number.contains('.') && !relative && center {
            value += 0.5;
        }
        Ok(Self { relative, value })
    }

    /// An integer with an optional `~` (whose offset may be fractional).
    pub fn parse_int(reader: &mut Reader<'_>) -> Result<Self, CommandError> {
        if reader.peek_is('^') {
            return Err(CommandError::simple("argument.pos.mixed").at(reader));
        }
        if !reader.can_read() {
            return Err(CommandError::simple("argument.pos.missing.int").at(reader));
        }
        let relative = Self::is_relative(reader);
        let value = if reader.can_read() && !reader.peek_is(' ') {
            if relative {
                reader.read_double()?
            } else {
                f64::from(reader.read_int()?)
            }
        } else {
            0.0
        };
        Ok(Self { relative, value })
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Coordinates {
    World {
        x: Coordinate,
        y: Coordinate,
        z: Coordinate,
    },
    Local {
        left: f64,
        up: f64,
        forwards: f64,
    },
}

impl Coordinates {
    pub fn position(&self, pose: &Pose) -> [f64; 3] {
        match *self {
            Self::World { x, y, z } => [
                x.get(pose.position[0]),
                y.get(pose.position[1]),
                z.get(pose.position[2]),
            ],
            Self::Local { left, up, forwards } => {
                let [yaw, pitch] = pose.rotation;
                let (y_sin, y_cos) = (yaw + 90.0).to_radians().sin_cos();
                let (x_sin, x_cos) = (-pitch).to_radians().sin_cos();
                let (x_sin_up, x_cos_up) = (-pitch + 90.0).to_radians().sin_cos();
                let f = [
                    f64::from(y_cos * x_cos),
                    f64::from(x_sin),
                    f64::from(y_sin * x_cos),
                ];
                let u = [
                    f64::from(y_cos * x_cos_up),
                    f64::from(x_sin_up),
                    f64::from(y_sin * x_cos_up),
                ];
                let l = [
                    -(f[1] * u[2] - f[2] * u[1]),
                    -(f[2] * u[0] - f[0] * u[2]),
                    -(f[0] * u[1] - f[1] * u[0]),
                ];
                let mut out = pose.anchor;
                for i in 0..3 {
                    out[i] += f[i] * forwards + u[i] * up + l[i] * left;
                }
                out
            }
        }
    }

    /// The block containing the position.
    pub fn block(&self, pose: &Pose) -> [i32; 3] {
        self.position(pose).map(|c| c.floor() as i32)
    }

    pub fn is_relative(&self) -> [bool; 3] {
        match self {
            Self::World { x, y, z } => [x.relative, y.relative, z.relative],
            Self::Local { .. } => [true; 3],
        }
    }

    fn expect_space(reader: &mut Reader<'_>, start: usize, key: &str) -> Result<(), CommandError> {
        if !reader.can_read() || !reader.peek_is(' ') {
            reader.set_cursor(start);
            return Err(CommandError::simple(key).at(reader));
        }
        reader.skip();
        Ok(())
    }

    fn parse_local(reader: &mut Reader<'_>) -> Result<Self, CommandError> {
        let start = reader.cursor();
        let read = |reader: &mut Reader<'_>| -> Result<f64, CommandError> {
            if !reader.can_read() {
                return Err(CommandError::simple("argument.pos.missing.double").at(reader));
            }
            if !reader.peek_is('^') {
                reader.set_cursor(start);
                return Err(CommandError::simple("argument.pos.mixed").at(reader));
            }
            reader.skip();
            if !reader.can_read() || reader.peek_is(' ') {
                return Ok(0.0);
            }
            reader.read_double()
        };
        let left = read(reader)?;
        Self::expect_space(reader, start, "argument.pos3d.incomplete")?;
        let up = read(reader)?;
        Self::expect_space(reader, start, "argument.pos3d.incomplete")?;
        let forwards = read(reader)?;
        Ok(Self::Local { left, up, forwards })
    }

    /// `vec3`: three doubles, whole absolute x and z centered in the block.
    pub fn parse_vec3(reader: &mut Reader<'_>, center: bool) -> Result<Self, CommandError> {
        if reader.peek_is('^') {
            return Self::parse_local(reader);
        }
        let start = reader.cursor();
        let x = Coordinate::parse_double(reader, center)?;
        Self::expect_space(reader, start, "argument.pos3d.incomplete")?;
        let y = Coordinate::parse_double(reader, false)?;
        Self::expect_space(reader, start, "argument.pos3d.incomplete")?;
        let z = Coordinate::parse_double(reader, center)?;
        Ok(Self::World { x, y, z })
    }

    /// `block_pos`: three integers.
    pub fn parse_block_pos(reader: &mut Reader<'_>) -> Result<Self, CommandError> {
        if reader.peek_is('^') {
            return Self::parse_local(reader);
        }
        let start = reader.cursor();
        let x = Coordinate::parse_int(reader)?;
        Self::expect_space(reader, start, "argument.pos3d.incomplete")?;
        let y = Coordinate::parse_int(reader)?;
        Self::expect_space(reader, start, "argument.pos3d.incomplete")?;
        let z = Coordinate::parse_int(reader)?;
        Ok(Self::World { x, y, z })
    }

    /// `vec2` / `column_pos`: x and z, with y left at the source's.
    pub fn parse_vec2(
        reader: &mut Reader<'_>,
        ints: bool,
        center: bool,
    ) -> Result<Self, CommandError> {
        let start = reader.cursor();
        if !reader.can_read() {
            return Err(CommandError::simple("argument.pos2d.incomplete").at(reader));
        }
        let read = |reader: &mut Reader<'_>| {
            if ints {
                Coordinate::parse_int(reader)
            } else {
                Coordinate::parse_double(reader, center)
            }
        };
        let x = read(reader)?;
        Self::expect_space(reader, start, "argument.pos2d.incomplete")?;
        let z = read(reader)?;
        Ok(Self::World {
            x,
            y: Coordinate {
                relative: true,
                value: 0.0,
            },
            z,
        })
    }
}

/// A `rotation` argument: yaw then pitch, each optionally relative.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rotation {
    pub yaw: Coordinate,
    pub pitch: Coordinate,
}

impl Rotation {
    pub const ZERO: Self = Self {
        yaw: Coordinate {
            relative: false,
            value: 0.0,
        },
        pitch: Coordinate {
            relative: false,
            value: 0.0,
        },
    };

    pub fn parse(reader: &mut Reader<'_>) -> Result<Self, CommandError> {
        let start = reader.cursor();
        if !reader.can_read() {
            return Err(CommandError::simple("argument.rotation.incomplete").at(reader));
        }
        let yaw = Coordinate::parse_double(reader, false)?;
        Coordinates::expect_space(reader, start, "argument.rotation.incomplete")?;
        let pitch = Coordinate::parse_double(reader, false)?;
        Ok(Self { yaw, pitch })
    }

    /// `[yaw, pitch]` for a source with `rotation`.
    pub fn get(&self, rotation: [f32; 2]) -> [f32; 2] {
        [
            self.yaw.get(f64::from(rotation[0])) as f32,
            self.pitch.get(f64::from(rotation[1])) as f32,
        ]
    }
}

/// An `angle` argument: one yaw, optionally relative.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Angle {
    pub relative: bool,
    pub value: f32,
}

impl Angle {
    pub fn parse(reader: &mut Reader<'_>) -> Result<Self, CommandError> {
        if !reader.can_read() {
            return Err(CommandError::simple("argument.angle.incomplete").at(reader));
        }
        let relative = Coordinate::is_relative(reader);
        let value = if !reader.can_read() || reader.peek_is(' ') {
            0.0
        } else {
            reader.read_float()?
        };
        if value.is_nan() || value.is_infinite() {
            return Err(CommandError::simple("argument.angle.invalid").at(reader));
        }
        Ok(Self { relative, value })
    }

    pub fn get(&self, yaw: f32) -> f32 {
        wrap_degrees(if self.relative {
            self.value + yaw
        } else {
            self.value
        })
    }
}

/// Wraps to `[-180, 180)` like the game.
/// The yaw and pitch that face `to` from `from`.
pub fn look_angles(from: [f64; 3], to: [f64; 3]) -> [f32; 2] {
    let dx = to[0] - from[0];
    let dy = to[1] - from[1];
    let dz = to[2] - from[2];
    let horizontal = (dx * dx + dz * dz).sqrt();
    let pitch = wrap_degrees(-(dy.atan2(horizontal).to_degrees()) as f32);
    let yaw = wrap_degrees(dz.atan2(dx).to_degrees() as f32 - 90.0);
    [yaw, pitch]
}

pub fn wrap_degrees(value: f32) -> f32 {
    let mut v = value % 360.0;
    if v >= 180.0 {
        v -= 360.0;
    }
    if v < -180.0 {
        v += 360.0;
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pose() -> Pose {
        Pose {
            position: [10.5, 64.0, -3.25],
            rotation: [90.0, 0.0],
            anchor: [10.5, 65.62, -3.25],
        }
    }

    fn key(err: CommandError) -> String {
        err.key().unwrap().to_owned()
    }

    #[tuff_core::watchdog]
    #[test]
    fn resolves_absolute_relative_and_local_positions() {
        let pose = pose();
        let abs = Coordinates::parse_vec3(&mut Reader::new("1 2 3"), true).unwrap();
        assert_eq!(abs.position(&pose), [1.5, 2.0, 3.5]);
        let frac = Coordinates::parse_vec3(&mut Reader::new("1.0 2 3.5"), true).unwrap();
        assert_eq!(frac.position(&pose), [1.0, 2.0, 3.5]);
        let rel = Coordinates::parse_vec3(&mut Reader::new("~ ~-1 ~2.5"), true).unwrap();
        assert_eq!(rel.position(&pose), [10.5, 63.0, -0.75]);
        assert_eq!(rel.is_relative(), [true; 3]);
        let block = Coordinates::parse_block_pos(&mut Reader::new("~ 70 ~-0.5")).unwrap();
        assert_eq!(block.block(&pose), [10, 70, -4]);
        let local = Coordinates::parse_vec3(&mut Reader::new("^ ^ ^2"), true).unwrap();
        let [x, y, z] = local.position(&pose);
        assert!((x - 8.5).abs() < 1e-6, "{x}");
        assert!((y - 65.62).abs() < 1e-6);
        assert!((z - (-3.25)).abs() < 1e-6, "{z}");
        let left = Coordinates::parse_vec3(&mut Reader::new("^1 ^ ^"), true).unwrap();
        let [x, _, z] = left.position(&pose);
        assert!((x - 10.5).abs() < 1e-6, "{x}");
        assert!((z - (-2.25)).abs() < 1e-6, "{z}");
    }

    #[tuff_core::watchdog]
    #[test]
    fn reports_coordinate_errors() {
        let vec3 =
            |input: &str| key(Coordinates::parse_vec3(&mut Reader::new(input), true).unwrap_err());
        assert_eq!(vec3("1 2"), "argument.pos3d.incomplete");
        assert_eq!(vec3("1 ^2 3"), "argument.pos.mixed");
        assert_eq!(vec3("^1 2 3"), "argument.pos.mixed");
        assert_eq!(vec3(""), "argument.pos.missing.double");
        assert_eq!(
            key(Coordinates::parse_block_pos(&mut Reader::new("1.5 2 3")).unwrap_err()),
            "parsing.int.invalid"
        );
        assert_eq!(
            key(Coordinates::parse_vec2(&mut Reader::new("1"), false, true).unwrap_err()),
            "argument.pos2d.incomplete"
        );
        assert_eq!(
            key(Rotation::parse(&mut Reader::new("1")).unwrap_err()),
            "argument.rotation.incomplete"
        );
        let rotation = Rotation::parse(&mut Reader::new("~10 -5")).unwrap();
        assert_eq!(rotation.get([90.0, 0.0]), [100.0, -5.0]);
        let angle = Angle::parse(&mut Reader::new("~100")).unwrap();
        assert_eq!(angle.get(90.0), -170.0);
        assert_eq!(wrap_degrees(180.0), -180.0);
        assert_eq!(wrap_degrees(-190.0), 170.0);
    }
}
