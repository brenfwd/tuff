import { memo } from "react";

const N = 9;
const W = 30; // half width of a block's top face
const H = 17; // half height of a block's top face
const D = 34; // block height

// Deterministic heightmap: a rounded plateau with a ridge and a pool of lava.
function heightAt(x: number, y: number) {
  const cx = x - (N - 1) / 2;
  const cy = y - (N - 1) / 2;
  const r = Math.hypot(cx, cy);
  if (r > 4.6) return 0;
  const ridge = Math.max(0, 2.2 - Math.abs(cx + cy * 0.6 - 1.2)) * 0.9;
  return Math.max(1, Math.round(4.4 - r * 0.8 + ridge * 1.3 + Math.sin(x * 1.7 + y) * 0.5));
}

type Block = { x: number; y: number; z: number; kind: "stone" | "tuff" | "ember" | "deep" };

function buildBlocks() {
  const blocks: Block[] = [];
  for (let x = 0; x < N; x++)
    for (let y = 0; y < N; y++) {
      const h = heightAt(x, y);
      const r = Math.hypot(x - (N - 1) / 2, y - (N - 1) / 2);
      for (let z = 0; z < h; z++) {
        const top = z === h - 1;
        const lava = top && h <= 2 && r > 2.6 && (x + y * 3) % 7 === 0;
        blocks.push({ x, y, z, kind: lava ? "ember" : top ? (x * y) % 5 === 1 ? "tuff" : "stone" : "deep" });
      }
    }
  return blocks.sort((a, b) => a.x + a.y - (b.x + b.y) || a.z - b.z);
}

const palette = {
  stone: ["var(--vx-top)", "var(--vx-left)", "var(--vx-right)"],
  tuff: ["var(--vx-tuff-top)", "var(--vx-left)", "var(--vx-right)"],
  deep: ["var(--vx-left)", "var(--vx-deep-left)", "var(--vx-deep-right)"],
  ember: ["#ff8a3d", "#c2410c", "#9a3412"],
};

export const VoxelIsland = memo(function VoxelIsland({ className = "" }: { className?: string }) {
  const blocks = buildBlocks();
  const ox = N * W;
  const oy = 7 * D;
  return (
    <svg viewBox={`0 0 ${N * W * 2} ${N * H * 2 + 8 * D}`} className={className} role="img" aria-label="An isometric island of stone blocks with glowing ember blocks">
      <defs>
        <radialGradient id="vx-glow" cx="50%" cy="50%" r="50%">
          <stop offset="0%" stopColor="var(--ember)" stopOpacity="0.55" />
          <stop offset="100%" stopColor="var(--ember)" stopOpacity="0" />
        </radialGradient>
      </defs>
      {blocks.map((b, i) => {
        const px = ox + (b.x - b.y) * W;
        const py = oy + (b.x + b.y) * H - b.z * D;
        const [top, left, right] = palette[b.kind];
        const delay = (b.x + b.y) * 45 + b.z * 70;
        return (
          <g key={i} className="voxel" style={{ animationDelay: `${delay}ms` }}>
            {b.kind === "ember" && <ellipse cx={px} cy={py + H} rx={W * 2.2} ry={H * 2.2} fill="url(#vx-glow)" className="ember-glow" />}
            <path d={`M${px} ${py}l${W} ${H}l${-W} ${H}l${-W} ${-H}z`} fill={top} />
            <path d={`M${px - W} ${py + H}l${W} ${H}v${D}l${-W} ${-H}z`} fill={left} />
            <path d={`M${px + W} ${py + H}l${-W} ${H}v${D}l${W} ${-H}z`} fill={right} />
            <path d={`M${px - W} ${py + H}l${W} ${H}l${W} ${-H}`} fill="none" stroke="var(--vx-edge)" strokeWidth="1" />
          </g>
        );
      })}
    </svg>
  );
});
