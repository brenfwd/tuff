//! Gradient noises (Perlin, the smeared Perlin of the old terrain noise,
//! simplex), octave stacks of them, and the normalized "normal noise" every
//! data-driven noise definition builds. All arithmetic follows the game's
//! float/double mix so values match bit for bit, both for single points and
//! for whole volumes (which the game computes along a slightly different
//! path, so the two are kept separate here too).

use crate::density::{DensityVolume, Interval};
use std::cell::Cell;
use tuff_item::random::{Random, RandomSource};

const GRADIENTS: [[i8; 3]; 16] = [
    [1, 1, 0],
    [-1, 1, 0],
    [1, -1, 0],
    [-1, -1, 0],
    [1, 0, 1],
    [-1, 0, 1],
    [1, 0, -1],
    [-1, 0, -1],
    [0, 1, 1],
    [0, -1, 1],
    [0, 1, -1],
    [0, -1, -1],
    [1, 1, 0],
    [0, -1, 1],
    [-1, 1, 0],
    [0, -1, -1],
];

/// `GRADIENTS` as f32 with a zero pad column; the components are -1, 0 and 1,
/// so every product and sum matches the integer table's converted values.
#[repr(align(64))]
struct GradientsF32([[f32; 4]; 16]);

static GRADIENTS_F32: GradientsF32 = {
    let mut table = [[0f32; 4]; 16];
    let mut i = 0;
    while i < 16 {
        let mut c = 0;
        while c < 3 {
            table[i][c] = GRADIENTS[i][c] as f32;
            c += 1;
        }
        i += 1;
    }
    GradientsF32(table)
};

const HALF_ROUND_OFF: f64 = 16777215.999999998;

fn wrap(x: f64) -> f64 {
    if (-HALF_ROUND_OFF..HALF_ROUND_OFF).contains(&x) {
        x
    } else {
        x - (x / 33554432.0 + 0.5).floor() * 33554432.0
    }
}

/// `x.floor() as i32` without the libm call baseline x86-64 needs (no
/// SSE4.1 round instruction): truncation is exact for every value in i32
/// range, so stepping down for negative non-integers gives the floor.
#[inline(always)]
pub fn floor(x: f64) -> i32 {
    if x > -2147483648.0 && x < 2147483647.0 {
        let t = x as i32;
        if (t as f64) > x { t - 1 } else { t }
    } else {
        x.floor() as i32
    }
}

pub fn smoothstep(x: f32) -> f32 {
    x * x * x * (x * (x * 6.0 - 15.0) + 10.0)
}

pub fn lerp(alpha: f32, a: f32, b: f32) -> f32 {
    a + alpha * (b - a)
}

pub fn lerp2(a1: f32, a2: f32, x00: f32, x10: f32, x01: f32, x11: f32) -> f32 {
    lerp(a2, lerp(a1, x00, x10), lerp(a1, x01, x11))
}

#[allow(clippy::too_many_arguments)]
pub fn lerp3(
    a1: f32,
    a2: f32,
    a3: f32,
    x000: f32,
    x100: f32,
    x010: f32,
    x110: f32,
    x001: f32,
    x101: f32,
    x011: f32,
    x111: f32,
) -> f32 {
    lerp(
        a3,
        lerp2(a1, a2, x000, x100, x010, x110),
        lerp2(a1, a2, x001, x101, x011, x111),
    )
}

#[inline(always)]
fn grad_dot(hash: i32, x: f32, y: f32, z: f32) -> f32 {
    let g = GRADIENTS_F32.0[(hash & 15) as usize];
    g[0] * x + g[1] * y + g[2] * z
}

#[cfg(test)]
fn grad_dot_int(hash: i32, x: f32, y: f32, z: f32) -> f32 {
    let g = GRADIENTS[(hash & 15) as usize];
    g[0] as f32 * x + g[1] as f32 * y + g[2] as f32 * z
}

#[cfg(test)]
fn grad_dot_xz(g: [i8; 3], x: f32, z: f32) -> f32 {
    g[0] as f32 * x + g[2] as f32 * z
}

/// The shared state of the gradient noises: a permutation table and a
/// random offset.
#[derive(Debug, Clone)]
struct Gradient {
    perms: [u8; 256],
    offset: [f64; 3],
}

impl Gradient {
    fn new(random: &mut Random, offset_scale: f64) -> Self {
        let offset = [
            random.next_double() * offset_scale,
            random.next_double() * offset_scale,
            random.next_double() * offset_scale,
        ];
        let mut perms = [0u8; 256];
        for (i, p) in perms.iter_mut().enumerate() {
            *p = i as u8;
        }
        for i in 0..256 {
            let offset = random.next_int_bounded(256 - i as i32) as usize;
            perms.swap(i, i + offset);
        }
        Self { perms, offset }
    }

    fn permute(&self, x: i32) -> i32 {
        self.perms[(x & 255) as usize] as i32
    }

    #[cfg(test)]
    fn grad(&self, x: i32) -> [i8; 3] {
        GRADIENTS[(self.permute(x) & 15) as usize]
    }

    #[allow(clippy::too_many_arguments)]
    fn sample_and_lerp(
        &self,
        x: i32,
        y: i32,
        z: i32,
        rx: f32,
        ry: f32,
        rz: f32,
        original_ry: f32,
    ) -> f32 {
        let x0 = self.permute(x);
        let x1 = self.permute(x + 1);
        let xy00 = self.permute(x0 + y);
        let xy01 = self.permute(x0 + y + 1);
        let xy10 = self.permute(x1 + y);
        let xy11 = self.permute(x1 + y + 1);
        let d000 = grad_dot(self.permute(xy00 + z), rx, ry, rz);
        let d100 = grad_dot(self.permute(xy10 + z), rx - 1.0, ry, rz);
        let d010 = grad_dot(self.permute(xy01 + z), rx, ry - 1.0, rz);
        let d110 = grad_dot(self.permute(xy11 + z), rx - 1.0, ry - 1.0, rz);
        let d001 = grad_dot(self.permute(xy00 + z + 1), rx, ry, rz - 1.0);
        let d101 = grad_dot(self.permute(xy10 + z + 1), rx - 1.0, ry, rz - 1.0);
        let d011 = grad_dot(self.permute(xy01 + z + 1), rx, ry - 1.0, rz - 1.0);
        let d111 = grad_dot(self.permute(xy11 + z + 1), rx - 1.0, ry - 1.0, rz - 1.0);
        lerp3(
            smoothstep(rx),
            smoothstep(original_ry),
            smoothstep(rz),
            d000,
            d100,
            d010,
            d110,
            d001,
            d101,
            d011,
            d111,
        )
    }

    /// Adds `amplitude * noise` over a volume, the way the game batches it:
    /// the xz part of each corner dot product is reused while the y cell
    /// stays the same. `fudge` supplies the smeared variant's y adjustment.
    /// The y run of each column is blended 8 lanes at a time; every lane
    /// performs the same float operations in the same order as the scalar
    /// path, so results are bit-identical.
    fn add_to_volume(
        &self,
        buffer: &mut [f32],
        volume: &DensityVolume,
        xz_scale: f64,
        y_scale: f64,
        amplitude: f32,
        fudge: Option<f64>,
    ) {
        #[cfg(target_arch = "x86_64")]
        if std::arch::is_x86_feature_detected!("avx2") {
            // SAFETY: the avx2 feature was just detected at runtime.
            return unsafe {
                self.add_to_volume_avx2(buffer, volume, xz_scale, y_scale, amplitude, fudge)
            };
        }
        self.add_to_volume_lanes(buffer, volume, xz_scale, y_scale, amplitude, fudge)
    }

    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "avx2")]
    unsafe fn add_to_volume_avx2(
        &self,
        buffer: &mut [f32],
        volume: &DensityVolume,
        xz_scale: f64,
        y_scale: f64,
        amplitude: f32,
        fudge: Option<f64>,
    ) {
        self.add_to_volume_lanes(buffer, volume, xz_scale, y_scale, amplitude, fudge)
    }

    #[inline(always)]
    fn add_to_volume_lanes(
        &self,
        buffer: &mut [f32],
        volume: &DensityVolume,
        xz_scale: f64,
        y_scale: f64,
        amplitude: f32,
        fudge: Option<f64>,
    ) {
        let size_y = volume.size_y as usize;
        let total = volume.size();
        let mut table = Y_SCRATCH.try_with(Cell::take).unwrap_or_default();
        table.fill(volume, y_scale, self.offset[1], fudge);
        let mut dxz = [0f32; 8];
        let mut gy = [0f32; 8];
        let mut column = self.column(volume, xz_scale, 0);
        let mut column_index = 0;
        let mut last_floor_y = i32::MIN;
        let mut iy = 0;
        let mut index = 0;
        while index < total {
            let n = (total - index).min(LANES);
            let mut p = [[0f32; LANES]; 8];
            let mut alpha_x = [0f32; LANES];
            let mut alpha_y = [0f32; LANES];
            let mut alpha_z = [0f32; LANES];
            for l in 0..n {
                if iy == size_y {
                    iy = 0;
                    column_index += 1;
                    column = self.column(volume, xz_scale, column_index);
                    last_floor_y = i32::MIN;
                }
                let floor_y = table.floor[iy];
                if last_floor_y != floor_y {
                    let xy00 = self.permute(column.x0 + floor_y);
                    let xy01 = self.permute(column.x0 + floor_y + 1);
                    let xy10 = self.permute(column.x1 + floor_y);
                    let xy11 = self.permute(column.x1 + floor_y + 1);
                    let (rx, rz, floor_z) = (column.rx, column.rz, column.floor_z);
                    let corners = [
                        (xy00 + floor_z, rx, rz),
                        (xy10 + floor_z, rx - 1.0, rz),
                        (xy01 + floor_z, rx, rz),
                        (xy11 + floor_z, rx - 1.0, rz),
                        (xy00 + floor_z + 1, rx, rz - 1.0),
                        (xy10 + floor_z + 1, rx - 1.0, rz - 1.0),
                        (xy01 + floor_z + 1, rx, rz - 1.0),
                        (xy11 + floor_z + 1, rx - 1.0, rz - 1.0),
                    ];
                    for (i, (hash, cx, cz)) in corners.into_iter().enumerate() {
                        let g = GRADIENTS_F32.0[(self.permute(hash) & 15) as usize];
                        dxz[i] = g[0] * cx + g[2] * cz;
                        gy[i] = g[1];
                    }
                    last_floor_y = floor_y;
                }
                let fy = table.fy[iy];
                let fym1 = table.fym1[iy];
                p[0][l] = dxz[0] + gy[0] * fy;
                p[1][l] = dxz[1] + gy[1] * fy;
                p[2][l] = dxz[2] + gy[2] * fym1;
                p[3][l] = dxz[3] + gy[3] * fym1;
                p[4][l] = dxz[4] + gy[4] * fy;
                p[5][l] = dxz[5] + gy[5] * fy;
                p[6][l] = dxz[6] + gy[6] * fym1;
                p[7][l] = dxz[7] + gy[7] * fym1;
                alpha_x[l] = column.alpha_x;
                alpha_y[l] = table.alpha[iy];
                alpha_z[l] = column.alpha_z;
                iy += 1;
            }
            let value = blend_lanes_impl(alpha_x, alpha_y, alpha_z, &p);
            for &v in &value[..n] {
                buffer[index] += amplitude * v;
                index += 1;
            }
        }
        debug_assert_eq!(iy, size_y);
        let _ = Y_SCRATCH.try_with(|s| s.set(table));
    }

    /// The x/z state shared by every point of one column, for the column
    /// at `index` in z-major order.
    #[inline(always)]
    fn column(&self, volume: &DensityVolume, xz_scale: f64, index: i32) -> Column {
        let iz = index / volume.size_x;
        let ix = index % volume.size_x;
        let z = wrap(volume.block_z(iz) as f64 * xz_scale) + self.offset[2];
        let floor_z = floor(z);
        let rz = (z - floor_z as f64) as f32;
        let x = wrap(volume.block_x(ix) as f64 * xz_scale) + self.offset[0];
        let floor_x = floor(x);
        let rx = (x - floor_x as f64) as f32;
        Column {
            x0: self.permute(floor_x),
            x1: self.permute(floor_x + 1),
            floor_z,
            rx,
            rz,
            alpha_x: smoothstep(rx),
            alpha_z: smoothstep(rz),
        }
    }
}

struct Column {
    x0: i32,
    x1: i32,
    floor_z: i32,
    rx: f32,
    rz: f32,
    alpha_x: f32,
    alpha_z: f32,
}

/// Everything about a column's y run that does not depend on x or z,
/// computed once per volume.
#[derive(Default)]
struct YTable {
    floor: Vec<i32>,
    fy: Vec<f32>,
    fym1: Vec<f32>,
    alpha: Vec<f32>,
}

thread_local! {
    static Y_SCRATCH: Cell<YTable> = Cell::new(YTable::default());
}

impl YTable {
    #[inline(always)]
    fn fill(&mut self, volume: &DensityVolume, y_scale: f64, offset: f64, fudge: Option<f64>) {
        let size = volume.size_y as usize;
        self.floor.clear();
        self.fy.clear();
        self.fym1.clear();
        self.alpha.clear();
        let mut start = 0;
        while start < size {
            let lanes = y_lanes(volume, start as i32, y_scale, offset);
            for l in 0..(size - start).min(LANES) {
                let ry = lanes.fraction[l];
                let fy = match fudge {
                    None => ry as f32,
                    Some(scale) => (ry - fudge_y(scale, lanes.original[l], ry)) as f32,
                };
                self.floor.push(lanes.floor[l]);
                self.fy.push(fy);
                self.fym1.push(fy - 1.0);
                self.alpha.push(lanes.alpha[l]);
            }
            start += LANES;
        }
    }
}

const LANES: usize = 8;

struct YLanes {
    original: [f64; LANES],
    floor: [i32; LANES],
    fraction: [f64; LANES],
    alpha: [f32; LANES],
}

/// The y cell, fraction and smoothstep of 8 consecutive block ys, written
/// without per-lane branches so it compiles to packed instructions. Lanes
/// past the end of the column are computed and ignored.
#[inline(always)]
fn y_lanes(volume: &DensityVolume, iy: i32, y_scale: f64, offset: f64) -> YLanes {
    let mut original = [0f64; LANES];
    let mut floored = [0f64; LANES];
    let mut fraction = [0f64; LANES];
    let mut alpha = [0f32; LANES];
    for l in 0..LANES {
        let o = volume.block_y(iy + l as i32) as f64 * y_scale;
        let y = wrap_select(o) + offset;
        let f = y.floor();
        let r = y - f;
        original[l] = o;
        floored[l] = f;
        fraction[l] = r;
        alpha[l] = smoothstep(r as f32);
    }
    let mut floor = [0i32; LANES];
    if floored.iter().all(|f| f.abs() < 2147483647.0) {
        for l in 0..LANES {
            // SAFETY: every lane was just checked to be finite and in i32 range.
            floor[l] = unsafe { floored[l].to_int_unchecked::<i32>() };
        }
    } else {
        for l in 0..LANES {
            floor[l] = floored[l] as i32;
        }
    }
    YLanes {
        original,
        floor,
        fraction,
        alpha,
    }
}

/// `wrap` with both arms computed and one selected, so lanes do not branch.
#[inline(always)]
fn wrap_select(x: f64) -> f64 {
    let far = x - (x / 33554432.0 + 0.5).floor() * 33554432.0;
    if (-HALF_ROUND_OFF..HALF_ROUND_OFF).contains(&x) {
        x
    } else {
        far
    }
}

#[inline(always)]
fn lerp_lanes(alpha: [f32; LANES], a: [f32; LANES], b: [f32; LANES]) -> [f32; LANES] {
    let mut out = [0f32; LANES];
    for l in 0..LANES {
        out[l] = a[l] + alpha[l] * (b[l] - a[l]);
    }
    out
}

#[inline(always)]
fn blend_lanes_impl(
    ax: [f32; LANES],
    ay: [f32; LANES],
    az: [f32; LANES],
    p: &[[f32; LANES]; 8],
) -> [f32; LANES] {
    let a = lerp_lanes(ax, p[0], p[1]);
    let b = lerp_lanes(ax, p[2], p[3]);
    let c = lerp_lanes(ax, p[4], p[5]);
    let d = lerp_lanes(ax, p[6], p[7]);
    let lo = lerp_lanes(ay, a, b);
    let hi = lerp_lanes(ay, c, d);
    lerp_lanes(az, lo, hi)
}

#[cfg(test)]
impl Gradient {
    #[allow(clippy::too_many_arguments)]
    fn sample_and_lerp_int(
        &self,
        x: i32,
        y: i32,
        z: i32,
        rx: f32,
        ry: f32,
        rz: f32,
        original_ry: f32,
    ) -> f32 {
        let x0 = self.permute(x);
        let x1 = self.permute(x + 1);
        let xy00 = self.permute(x0 + y);
        let xy01 = self.permute(x0 + y + 1);
        let xy10 = self.permute(x1 + y);
        let xy11 = self.permute(x1 + y + 1);
        let d000 = grad_dot_int(self.permute(xy00 + z), rx, ry, rz);
        let d100 = grad_dot_int(self.permute(xy10 + z), rx - 1.0, ry, rz);
        let d010 = grad_dot_int(self.permute(xy01 + z), rx, ry - 1.0, rz);
        let d110 = grad_dot_int(self.permute(xy11 + z), rx - 1.0, ry - 1.0, rz);
        let d001 = grad_dot_int(self.permute(xy00 + z + 1), rx, ry, rz - 1.0);
        let d101 = grad_dot_int(self.permute(xy10 + z + 1), rx - 1.0, ry, rz - 1.0);
        let d011 = grad_dot_int(self.permute(xy01 + z + 1), rx, ry - 1.0, rz - 1.0);
        let d111 = grad_dot_int(self.permute(xy11 + z + 1), rx - 1.0, ry - 1.0, rz - 1.0);
        lerp3(
            smoothstep(rx),
            smoothstep(original_ry),
            smoothstep(rz),
            d000,
            d100,
            d010,
            d110,
            d001,
            d101,
            d011,
            d111,
        )
    }

    /// Reference path. Adds `amplitude * noise` over a volume, the way the game batches it:
    /// the xz part of each corner dot product is reused while the y cell
    /// stays the same. `fudge` supplies the smeared variant's y adjustment.
    fn add_to_volume_scalar(
        &self,
        buffer: &mut [f32],
        volume: &DensityVolume,
        xz_scale: f64,
        y_scale: f64,
        amplitude: f32,
        fudge: Option<f64>,
    ) {
        let mut dxz = [0f32; 8];
        let mut gy = [0f32; 8];
        let mut index = 0;
        for iz in 0..volume.size_z {
            let z = wrap(volume.block_z(iz) as f64 * xz_scale) + self.offset[2];
            let floor_z = floor(z);
            let rz = (z - floor_z as f64) as f32;
            let alpha_z = smoothstep(rz);
            for ix in 0..volume.size_x {
                let x = wrap(volume.block_x(ix) as f64 * xz_scale) + self.offset[0];
                let floor_x = floor(x);
                let rx = (x - floor_x as f64) as f32;
                let x0 = self.permute(floor_x);
                let x1 = self.permute(floor_x + 1);
                let alpha_x = smoothstep(rx);
                let mut last_floor_y = i32::MIN;
                for iy in 0..volume.size_y {
                    let original_y = volume.block_y(iy) as f64 * y_scale;
                    let y = wrap(original_y) + self.offset[1];
                    let floor_y = floor(y);
                    let ry = y - floor_y as f64;
                    let alpha_y = smoothstep(ry as f32);
                    if last_floor_y != floor_y {
                        let xy00 = self.permute(x0 + floor_y);
                        let xy01 = self.permute(x0 + floor_y + 1);
                        let xy10 = self.permute(x1 + floor_y);
                        let xy11 = self.permute(x1 + floor_y + 1);
                        let corners = [
                            (xy00 + floor_z, rx, rz),
                            (xy10 + floor_z, rx - 1.0, rz),
                            (xy01 + floor_z, rx, rz),
                            (xy11 + floor_z, rx - 1.0, rz),
                            (xy00 + floor_z + 1, rx, rz - 1.0),
                            (xy10 + floor_z + 1, rx - 1.0, rz - 1.0),
                            (xy01 + floor_z + 1, rx, rz - 1.0),
                            (xy11 + floor_z + 1, rx - 1.0, rz - 1.0),
                        ];
                        for (i, (hash, cx, cz)) in corners.into_iter().enumerate() {
                            let g = self.grad(hash);
                            dxz[i] = grad_dot_xz(g, cx, cz);
                            gy[i] = g[1] as f32;
                        }
                        last_floor_y = floor_y;
                    }
                    let fy = match fudge {
                        None => ry as f32,
                        Some(scale) => (ry - fudge_y(scale, original_y, ry)) as f32,
                    };
                    let value = lerp3(
                        alpha_x,
                        alpha_y,
                        alpha_z,
                        dxz[0] + gy[0] * fy,
                        dxz[1] + gy[1] * fy,
                        dxz[2] + gy[2] * (fy - 1.0),
                        dxz[3] + gy[3] * (fy - 1.0),
                        dxz[4] + gy[4] * fy,
                        dxz[5] + gy[5] * fy,
                        dxz[6] + gy[6] * (fy - 1.0),
                        dxz[7] + gy[7] * (fy - 1.0),
                    );
                    buffer[index] += amplitude * value;
                    index += 1;
                }
            }
        }
    }
}

fn fudge_y(scale: f64, original_y: f64, relative_y: f64) -> f64 {
    let limit = if original_y >= 0.0 && original_y < relative_y {
        original_y
    } else {
        relative_y
    };
    floor(limit / scale + 1.0000000116860974E-7) as f64 * scale
}

/// One octave: plain Perlin noise, or the smeared variant the old terrain
/// noise uses, which flattens y inside each `fudge` step.
#[derive(Debug, Clone)]
pub struct Octave {
    gradient: Gradient,
    fudge: Option<f64>,
}

impl Octave {
    pub fn perlin(random: &mut Random) -> Self {
        Self {
            gradient: Gradient::new(random, 256.0),
            fudge: None,
        }
    }

    pub fn smeared(random: &mut Random, fudge: f64) -> Self {
        Self {
            gradient: Gradient::new(random, 256.0),
            fudge: Some(fudge),
        }
    }

    pub fn range(&self) -> Interval {
        match self.fudge {
            None => Interval::symmetric(2.0),
            Some(fudge) => Interval::symmetric((fudge.abs() + 2.0) as f32),
        }
    }

    pub fn get(&self, x: f64, y: f64, z: f64) -> f32 {
        let g = &self.gradient;
        let px = wrap(x) + g.offset[0];
        let py = wrap(y) + g.offset[1];
        let pz = wrap(z) + g.offset[2];
        let (fx, fy, fz) = (floor(px), floor(py), floor(pz));
        let rx = (px - fx as f64) as f32;
        let rz = (pz - fz as f64) as f32;
        match self.fudge {
            None => {
                let ry = (py - fy as f64) as f32;
                g.sample_and_lerp(fx, fy, fz, rx, ry, rz, ry)
            }
            Some(scale) => {
                let ry = py - fy as f64;
                let fudged = (ry - fudge_y(scale, y, ry)) as f32;
                g.sample_and_lerp(fx, fy, fz, rx, fudged, rz, ry as f32)
            }
        }
    }

    pub fn get_2d(&self, x: f64, y: f64) -> f32 {
        self.get(wrap(x), 0.0, wrap(y))
    }

    pub fn add_to_volume(
        &self,
        buffer: &mut [f32],
        volume: &DensityVolume,
        xz_scale: f64,
        y_scale: f64,
        amplitude: f32,
    ) {
        self.gradient
            .add_to_volume(buffer, volume, xz_scale, y_scale, amplitude, self.fudge);
    }

    #[cfg(test)]
    fn get_int(&self, x: f64, y: f64, z: f64) -> f32 {
        let g = &self.gradient;
        let px = wrap(x) + g.offset[0];
        let py = wrap(y) + g.offset[1];
        let pz = wrap(z) + g.offset[2];
        let (fx, fy, fz) = (floor(px), floor(py), floor(pz));
        let rx = (px - fx as f64) as f32;
        let rz = (pz - fz as f64) as f32;
        match self.fudge {
            None => {
                let ry = (py - fy as f64) as f32;
                g.sample_and_lerp_int(fx, fy, fz, rx, ry, rz, ry)
            }
            Some(scale) => {
                let ry = py - fy as f64;
                let fudged = (ry - fudge_y(scale, y, ry)) as f32;
                g.sample_and_lerp_int(fx, fy, fz, rx, fudged, rz, ry as f32)
            }
        }
    }

    #[cfg(test)]
    pub fn perm(&self, i: usize) -> u8 {
        self.gradient.perms[i]
    }

    #[cfg(test)]
    pub fn offset(&self) -> [f64; 3] {
        self.gradient.offset
    }
}

#[derive(Debug, Clone)]
struct Layer {
    octave: Octave,
    frequency: f64,
    amplitude: f32,
}

/// A weighted sum of octaves, sampled at scaled coordinates.
#[derive(Debug, Clone)]
pub struct NoiseStack {
    layers: Vec<Layer>,
    range: Interval,
}

#[derive(Debug, Default)]
pub struct StackBuilder {
    layers: Vec<Layer>,
}

impl StackBuilder {
    pub fn add(mut self, octave: Octave, frequency: f64, amplitude: f32) -> Self {
        self.layers.push(Layer {
            octave,
            frequency,
            amplitude,
        });
        self
    }

    pub fn add_stack(mut self, stack: NoiseStack, frequency: f64, amplitude: f32) -> Self {
        for layer in stack.layers {
            self.layers.push(Layer {
                frequency: layer.frequency * frequency,
                amplitude: layer.amplitude * amplitude,
                octave: layer.octave,
            });
        }
        self
    }

    pub fn build(self) -> NoiseStack {
        let mut range = Interval::exact(0.0);
        for layer in &self.layers {
            let layer_range =
                Interval::product(layer.octave.range(), Interval::exact(layer.amplitude));
            range = Interval::sum(range, layer_range);
        }
        NoiseStack {
            layers: self.layers,
            range,
        }
    }
}

impl NoiseStack {
    pub fn builder() -> StackBuilder {
        StackBuilder::default()
    }

    pub fn range(&self) -> Interval {
        self.range
    }

    pub fn get(&self, x: f64, y: f64, z: f64) -> f32 {
        let mut value = 0.0f32;
        for layer in &self.layers {
            let f = layer.frequency;
            value += layer.amplitude * layer.octave.get(x * f, y * f, z * f);
        }
        value
    }

    pub fn get_2d(&self, x: f64, y: f64) -> f32 {
        let mut value = 0.0f32;
        for layer in &self.layers {
            let f = layer.frequency;
            value += layer.amplitude * layer.octave.get_2d(x * f, y * f);
        }
        value
    }

    pub fn add_to_volume(
        &self,
        buffer: &mut [f32],
        volume: &DensityVolume,
        xz_scale: f64,
        y_scale: f64,
        amplitude: f32,
    ) {
        for layer in &self.layers {
            let f = layer.frequency;
            layer.octave.add_to_volume(
                buffer,
                volume,
                xz_scale * f,
                y_scale * f,
                amplitude * layer.amplitude,
            );
        }
    }

    #[cfg(test)]
    pub fn layer(&self, index: usize) -> &Octave {
        &self.layers[index].octave
    }
}

/// Simplex noise, used by the end's outer islands.
#[derive(Debug, Clone)]
pub struct Simplex {
    gradient: Gradient,
}

const SQRT_3: f64 = 1.7320508075688772;
const F2: f64 = 0.5 * (SQRT_3 - 1.0);
const G2: f64 = (3.0 - SQRT_3) / 6.0;

impl Simplex {
    pub fn new(random: &mut Random, discard_offset: bool) -> Self {
        Self {
            gradient: Gradient::new(random, if discard_offset { 0.0 } else { 256.0 }),
        }
    }

    fn corner(&self, index: i32, x: f64, y: f64, z: f64, base: f64) -> f64 {
        let t0 = base - x * x - y * y - z * z;
        if t0 < 0.0 {
            0.0
        } else {
            let t1 = t0 * t0;
            let g = GRADIENTS[index as usize];
            t1 * t1 * (g[0] as f64 * x + g[1] as f64 * y + g[2] as f64 * z)
        }
    }

    pub fn get_2d(&self, x: f64, y: f64) -> f32 {
        let g = &self.gradient;
        let xin = x + g.offset[0];
        let yin = y + g.offset[1];
        let s = (xin + yin) * F2;
        let i = floor(xin + s);
        let j = floor(yin + s);
        let t = (i.wrapping_add(j)) as f64 * G2;
        let x0 = xin - (i as f64 - t);
        let y0 = yin - (j as f64 - t);
        let (i1, j1) = if x0 > y0 { (1, 0) } else { (0, 1) };
        let x1 = x0 - i1 as f64 + G2;
        let y1 = y0 - j1 as f64 + G2;
        let x2 = x0 - 1.0 + 2.0 * G2;
        let y2 = y0 - 1.0 + 2.0 * G2;
        let ii = i & 255;
        let jj = j & 255;
        let gi0 = g.permute(ii + g.permute(jj)) % 12;
        let gi1 = g.permute(ii + i1 + g.permute(jj + j1)) % 12;
        let gi2 = g.permute(ii + 1 + g.permute(jj + 1)) % 12;
        let n0 = self.corner(gi0, x0, y0, 0.0, 0.5);
        let n1 = self.corner(gi1, x1, y1, 0.0, 0.5);
        let n2 = self.corner(gi2, x2, y2, 0.0, 0.5);
        (70.0 * (n0 + n1 + n2)) as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn wrap_keeps_small_coordinates() {
        assert_eq!(wrap(1234.5), 1234.5);
        assert_eq!(wrap(16777216.0), 16777216.0 - 33554432.0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn floor_matches_the_library_floor() {
        let edges = [
            0.0,
            -0.0,
            0.5,
            -0.5,
            1.0,
            -1.0,
            -1.0 - f64::EPSILON,
            -f64::MIN_POSITIVE,
            33554431.999999996,
            -33554431.999999996,
            2147483646.5,
            -2147483647.5,
            2147483647.0,
            -2147483648.0,
            -2147483648.5,
            1e12,
            -1e12,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NAN,
        ];
        for x in edges {
            assert_eq!(floor(x), x.floor() as i32, "{x}");
        }
        let mut state = 0x9e3779b97f4a7c15u64;
        for _ in 0..1_000_000 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let x = (state as i64 as f64) / (1u64 << (state % 50)) as f64;
            assert_eq!(floor(x), x.floor() as i32, "{x}");
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn lane_volume_matches_scalar_bit_for_bit() {
        let mut random = Random::Xoroshiro(tuff_item::random::XoroshiroRandom::new(12345));
        for (case, fudge) in [None, Some(0.125), Some(8.0)].into_iter().enumerate() {
            let octave = match fudge {
                None => Octave::perlin(&mut random),
                Some(f) => Octave::smeared(&mut random, f),
            };
            for (size, min, step, scales) in [
                ([5, 37, 3], [-33, -64, 100], [4, 1, 4], (0.0123, 0.0456)),
                ([4, 11, 6], [1000, 70, -2000], [1, 8, 1], (1.7, 0.31)),
                (
                    [3, 8, 3],
                    [3_000_000, 10, -5_000_000],
                    [16, 3, 16],
                    (97.3, 13.7),
                ),
            ] {
                let volume = DensityVolume::stepped(size, min, step);
                let len = (size[0] * size[1] * size[2]) as usize;
                let amp = 0.37 + case as f32;
                let mut fast = vec![0.5f32; len];
                let mut slow = vec![0.5f32; len];
                octave.gradient.add_to_volume(
                    &mut fast,
                    &volume,
                    scales.0,
                    scales.1,
                    amp,
                    octave.fudge,
                );
                octave.gradient.add_to_volume_scalar(
                    &mut slow,
                    &volume,
                    scales.0,
                    scales.1,
                    amp,
                    octave.fudge,
                );
                for (a, b) in fast.iter().zip(&slow) {
                    assert_eq!(a.to_bits(), b.to_bits());
                }
            }
        }
    }

    struct Xorshift(u64);

    impl Xorshift {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        fn range(&mut self, lo: i32, hi: i32) -> i32 {
            lo + (self.next() % (hi - lo + 1) as u64) as i32
        }

        fn unit(&mut self) -> f64 {
            (self.next() >> 11) as f64 / (1u64 << 53) as f64
        }

        fn coordinate(&mut self) -> f64 {
            match self.next() % 12 {
                0 => 0.0,
                1 => -0.0,
                2 => f64::from(self.range(-40, 40)),
                3 => (self.unit() - 0.5) * 1e8,
                4 => (self.unit() - 0.5) * 6.7e7,
                5 => 16777216.0 + self.range(-3, 3) as f64,
                6 => f64::NAN,
                7 => f64::INFINITY,
                8 => -3.0e9,
                _ => (self.unit() - 0.5) * 4000.0,
            }
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn point_noise_matches_the_integer_gradient_path_bit_for_bit() {
        let mut rng = Xorshift(0x2545f4914f6cdd1d);
        for seed in 0..24 {
            let mut random = Random::Xoroshiro(tuff_item::random::XoroshiroRandom::new(seed));
            let octave = match seed % 3 {
                0 => Octave::perlin(&mut random),
                1 => Octave::smeared(&mut random, 0.125),
                _ => Octave::smeared(&mut random, 8.0),
            };
            for _ in 0..20_000 {
                let (x, y, z) = (rng.coordinate(), rng.coordinate(), rng.coordinate());
                assert_eq!(
                    octave.get(x, y, z).to_bits(),
                    octave.get_int(x, y, z).to_bits(),
                    "{x} {y} {z}"
                );
            }
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn random_volumes_match_the_scalar_path_bit_for_bit() {
        let mut rng = Xorshift(0x9e3779b97f4a7c15);
        for case in 0..400 {
            let mut random =
                Random::Xoroshiro(tuff_item::random::XoroshiroRandom::new(1000 + case));
            let octave = match case % 3 {
                0 => Octave::perlin(&mut random),
                1 => Octave::smeared(&mut random, 0.125),
                _ => Octave::smeared(&mut random, 8.0),
            };
            let size = match case % 5 {
                0 => [5, 49, 5],
                1 => [rng.range(1, 6), rng.range(1, 20), rng.range(1, 6)],
                2 => [rng.range(1, 3), rng.range(1, 400), rng.range(1, 3)],
                3 => [rng.range(1, 17), rng.range(1, 9), rng.range(1, 17)],
                _ => [rng.range(1, 6), 8 * rng.range(1, 6) + rng.range(-1, 1), 1],
            };
            let min = match case % 4 {
                0 => [-64 * 4 + rng.range(0, 8) * 4, -64, rng.range(-1000, 1000)],
                1 => [
                    rng.range(-3_000_000, 3_000_000),
                    rng.range(-70, 330),
                    rng.range(-3_000_000, 3_000_000),
                ],
                2 => [16777216 - 4, rng.range(-64, 64), -16777216 - 4],
                _ => [
                    rng.range(-50, 50),
                    rng.range(-2_000_000, 2_000_000),
                    rng.range(-50, 50),
                ],
            };
            let step = [
                rng.range(1, 16),
                if case % 4 == 0 { 8 } else { rng.range(1, 9) },
                rng.range(1, 16),
            ];
            let scales = (
                [0.0123, 1.7, 97.3, 0.0005, 3.0][case as usize % 5] * (0.5 + rng.unit()),
                [0.0456, 0.31, 13.7, 0.0011, 5.0][(case as usize / 5) % 5] * (0.5 + rng.unit()),
            );
            let volume = DensityVolume::stepped(size, min, step);
            let len = volume.size();
            let amp = (rng.unit() * 4.0 - 2.0) as f32;
            let mut fast = vec![0.25f32; len];
            let mut slow = vec![0.25f32; len];
            octave.gradient.add_to_volume(
                &mut fast,
                &volume,
                scales.0,
                scales.1,
                amp,
                octave.fudge,
            );
            octave.gradient.add_to_volume_scalar(
                &mut slow,
                &volume,
                scales.0,
                scales.1,
                amp,
                octave.fudge,
            );
            for (i, (a, b)) in fast.iter().zip(&slow).enumerate() {
                assert_eq!(a.to_bits(), b.to_bits(), "case {case} index {i}");
            }
        }
    }
}
