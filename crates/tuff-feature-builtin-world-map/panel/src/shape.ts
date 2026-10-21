// The pre-generation shape on the map: which chunks it covers (the same
// rule as the server's) and how its centre and resize handles are dragged.

import { type View, screenOf } from "./view";

export type ShapeKind = "circle" | "square";
export const MAX_RADIUS = 100_000;
const HANDLE_PX = 8;

export interface Area {
  x: number;
  z: number;
  radius: number;
  shape: ShapeKind;
}

export type Grip = "centre" | "edge";

/** The distance from `v` to the nearest block of chunk coordinate `c`. */
function gap(v: number, c: number): number {
  const lo = c * 16;
  const hi = lo + 15;
  return v < lo ? lo - v : v > hi ? v - hi : 0;
}

/** Whether any block of chunk (`cx`, `cz`) is inside the area. */
export function containsChunk(area: Area, cx: number, cz: number): boolean {
  const dx = gap(area.x, cx);
  const dz = gap(area.z, cz);
  const r = area.radius;
  return area.shape === "square" ? dx <= r && dz <= r : dx * dx + dz * dz <= r * r;
}

/** How many chunks the area covers. */
export function chunkCount(area: Area): number {
  const x0 = Math.floor((area.x - area.radius) / 16);
  const x1 = Math.floor((area.x + area.radius) / 16);
  const z0 = Math.floor((area.z - area.radius) / 16);
  const z1 = Math.floor((area.z + area.radius) / 16);
  if (area.shape === "square") return (x1 - x0 + 1) * (z1 - z0 + 1);
  let total = 0;
  const r2 = area.radius * area.radius;
  for (let cz = z0; cz <= z1; cz++) {
    const dz = gap(area.z, cz);
    const w = Math.floor(Math.sqrt(r2 - dz * dz));
    const width = Math.floor((area.x + w) / 16) - Math.floor((area.x - w) / 16) + 1;
    total += width;
  }
  return total;
}

/** Screen points of the resize handles: four for a circle, eight for a
 * square. */
export function handles(area: Area, view: View, width: number, height: number) {
  const r = area.radius;
  const points =
    area.shape === "circle"
      ? [
          [r, 0],
          [0, r],
          [-r, 0],
          [0, -r],
        ]
      : [
          [r, 0],
          [r, r],
          [0, r],
          [-r, r],
          [-r, 0],
          [-r, -r],
          [0, -r],
          [r, -r],
        ];
  return points.map(([dx, dz]) => screenOf(view, width, height, area.x + 0.5 + dx, area.z + 0.5 + dz));
}

/** What a press at (`px`, `py`) grabs: a handle, the centre, or nothing. */
export function grab(area: Area, view: View, width: number, height: number, px: number, py: number): Grip | null {
  const near = (p: { left: number; top: number }) => Math.hypot(p.left - px, p.top - py) <= HANDLE_PX;
  if (handles(area, view, width, height).some(near)) return "edge";
  if (near(screenOf(view, width, height, area.x + 0.5, area.z + 0.5))) return "centre";
  return null;
}

/** The area after dragging `grip` to block (`x`, `z`). */
export function dragTo(area: Area, grip: Grip, x: number, z: number): Area {
  if (grip === "centre") return { ...area, x, z };
  const dx = Math.abs(x - area.x);
  const dz = Math.abs(z - area.z);
  const radius = area.shape === "circle" ? Math.round(Math.hypot(dx, dz)) : Math.max(dx, dz);
  return { ...area, radius: Math.min(MAX_RADIUS, radius) };
}
