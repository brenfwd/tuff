import { useEffect, useRef, type CSSProperties } from "react";
import { prefersReducedMotion } from "../lib/motion";

export type CubeSpec = {
  side: "left" | "right";
  top: string;
  size: number;
  offset?: number;
  /** Degrees of rotation per full pass of the section through the viewport. */
  rate?: number;
  axis?: [number, number, number];
  tilt?: number;
  mobile?: boolean;
};

const faces = ["front", "back", "right", "left", "top", "bottom"] as const;

const pointer = { x: 0, y: 0, active: false };
let pointerBound = false;
function bindPointer() {
  if (pointerBound) return;
  pointerBound = true;
  if (!matchMedia("(hover: hover) and (pointer: fine)").matches) return;
  pointer.active = true;
  addEventListener(
    "pointermove",
    (e) => {
      pointer.x = (e.clientX / innerWidth) * 2 - 1;
      pointer.y = (e.clientY / innerHeight) * 2 - 1;
    },
    { passive: true },
  );
}

const smooth = (t: number) => t * t * (3 - 2 * t);
const clamp01 = (t: number) => Math.min(1, Math.max(0, t));

/**
 * Dark cubes at a section's edges. Rotation and scale follow the section's scroll
 * progress; on fine pointers the pointer adds a small depth-weighted tilt.
 */
export function Cubes({ cubes }: { cubes: CubeSpec[] }) {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const root = ref.current;
    if (!root || prefersReducedMotion()) return;
    bindPointer();
    const section = root.parentElement ?? root;
    const slots = [...root.querySelectorAll<HTMLElement>(".cube-slot")];
    const items = slots.map((slot, i) => ({
      wrap: slot.firstElementChild as HTMLElement,
      cube: slot.querySelector<HTMLElement>(".cube")!,
      spec: cubes[i],
      depth: cubes[i].size / 160,
    }));
    const coarse = matchMedia("(pointer: coarse)").matches;
    const px = { x: 0, y: 0 };
    let frame = 0;
    let visible = false;

    const tick = () => {
      frame = 0;
      const rect = section.getBoundingClientRect();
      const vh = innerHeight;
      const progress = clamp01((vh - rect.top) / (vh + rect.height));
      px.x += ((pointer.active ? pointer.x : 0) - px.x) * 0.08;
      px.y += ((pointer.active ? pointer.y : 0) - px.y) * 0.08;
      for (const { wrap, cube, spec, depth } of items) {
        const slotRect = wrap.getBoundingClientRect();
        const enter = smooth(clamp01((vh - slotRect.top) / (vh * 0.45)));
        const scale = 0.55 + 0.45 * enter;
        const shift = coarse ? 0 : depth;
        wrap.style.opacity = `calc(var(--cube-opacity) * ${enter.toFixed(3)})`;
        wrap.style.transform = `translate3d(${(px.x * 14 * shift).toFixed(2)}px, ${(px.y * 10 * shift).toFixed(2)}px, 0) scale(${scale.toFixed(3)})`;
        const [ax, ay, az] = spec.axis ?? [0.4, 1, 0.2];
        const r = progress * (spec.rate ?? 220);
        const tilt = spec.tilt ?? -24;
        cube.style.transform = `rotateX(${(tilt + r * ax - px.y * 12 * depth).toFixed(2)}deg) rotateY(${(35 + r * ay + px.x * 16 * depth).toFixed(2)}deg) rotateZ(${(r * az).toFixed(2)}deg)`;
      }
      const settling = Math.abs((pointer.active ? pointer.x : 0) - px.x) + Math.abs((pointer.active ? pointer.y : 0) - px.y) > 0.002;
      if (visible && settling) frame = requestAnimationFrame(tick);
    };
    const request = () => {
      if (visible && !frame) frame = requestAnimationFrame(tick);
    };
    const io = new IntersectionObserver(([e]) => {
      visible = e.isIntersecting;
      request();
    });
    io.observe(section);
    addEventListener("scroll", request, { passive: true });
    addEventListener("resize", request, { passive: true });
    if (pointer.active) addEventListener("pointermove", request, { passive: true });
    return () => {
      io.disconnect();
      removeEventListener("scroll", request);
      removeEventListener("resize", request);
      removeEventListener("pointermove", request);
      cancelAnimationFrame(frame);
    };
  }, [cubes]);
  return (
    <div ref={ref} className="pointer-events-none absolute inset-0 -z-10 overflow-hidden" aria-hidden="true">
      {cubes.map((c, i) => (
        <div
          key={i}
          className={`cube-slot${c.mobile ? "" : " cube-desktop"}`}
          style={{ top: c.top, [c.side]: `${-(c.offset ?? c.size * 0.35)}px`, "--size": `${c.size}px`, "--tilt": `${c.tilt ?? -24}deg` } as CSSProperties}
        >
          <div className="cube-scale">
            <div className="cube">
              {faces.map((f) => (
                <span key={f} className={`cube-face cube-${f}`} />
              ))}
            </div>
          </div>
        </div>
      ))}
    </div>
  );
}
