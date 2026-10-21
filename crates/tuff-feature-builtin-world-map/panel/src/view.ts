// Pure map view math: where the camera is, which tiles cover the screen
// and where each one is drawn. Blocks are world coordinates; screen pixels
// are canvas CSS pixels.

export const TILE = 512;
export const MAX_LEVEL = 6;
export const MIN_SCALE = 1 / 64;
export const MAX_SCALE = 8;

export interface View {
  dim: number;
  /** The block at the centre of the canvas. */
  x: number;
  z: number;
  /** Screen pixels per block. */
  scale: number;
}

export interface TileRef {
  dim: number;
  level: number;
  x: number;
  z: number;
}

export interface Placed extends TileRef {
  left: number;
  top: number;
  size: number;
}

export function clampScale(scale: number): number {
  return Math.min(MAX_SCALE, Math.max(MIN_SCALE, scale));
}

/** The pyramid level whose pixels are closest to one screen pixel, never
 * finer than a block. */
export function levelFor(scale: number): number {
  if (scale >= 1) return 0;
  return Math.min(MAX_LEVEL, Math.max(0, Math.floor(Math.log2(1 / scale) + 1e-9)));
}

export function tileKey(t: TileRef): string {
  return `${t.dim}/${t.level}/${t.x}/${t.z}`;
}

export function tileSpan(level: number): number {
  return TILE * 2 ** level;
}

/** The blocks the canvas shows. */
export function bounds(view: View, width: number, height: number) {
  const halfW = width / 2 / view.scale;
  const halfH = height / 2 / view.scale;
  return {
    minX: view.x - halfW,
    minZ: view.z - halfH,
    maxX: view.x + halfW,
    maxZ: view.z + halfH,
  };
}

/** Every tile covering the canvas at the view's level, with its screen
 * rectangle. */
export function visibleTiles(view: View, width: number, height: number): Placed[] {
  const level = levelFor(view.scale);
  const span = tileSpan(level);
  const b = bounds(view, width, height);
  const out: Placed[] = [];
  const x0 = Math.floor(b.minX / span);
  const x1 = Math.floor((b.maxX - 1e-9) / span);
  const z0 = Math.floor(b.minZ / span);
  const z1 = Math.floor((b.maxZ - 1e-9) / span);
  for (let z = z0; z <= z1; z++) {
    for (let x = x0; x <= x1; x++) {
      out.push({
        dim: view.dim,
        level,
        x,
        z,
        left: (x * span - b.minX) * view.scale,
        top: (z * span - b.minZ) * view.scale,
        size: span * view.scale,
      });
    }
  }
  return out;
}

/** The block under a canvas point. */
export function blockAt(view: View, width: number, height: number, px: number, py: number) {
  return {
    x: Math.floor(view.x + (px - width / 2) / view.scale),
    z: Math.floor(view.z + (py - height / 2) / view.scale),
  };
}

/** The canvas point a world position is drawn at. */
export function screenOf(view: View, width: number, height: number, x: number, z: number) {
  return {
    left: (x - view.x) * view.scale + width / 2,
    top: (z - view.z) * view.scale + height / 2,
  };
}

/** Zooms by `factor` keeping the block under (`px`, `py`) in place. */
export function zoomAt(view: View, width: number, height: number, px: number, py: number, factor: number): View {
  const scale = clampScale(view.scale * factor);
  const wx = view.x + (px - width / 2) / view.scale;
  const wz = view.z + (py - height / 2) / view.scale;
  return {
    ...view,
    scale,
    x: wx - (px - width / 2) / scale,
    z: wz - (py - height / 2) / scale,
  };
}

/** Moves the view by a drag of (`dx`, `dy`) screen pixels. */
export function panBy(view: View, dx: number, dy: number): View {
  return { ...view, x: view.x - dx / view.scale, z: view.z - dy / view.scale };
}
