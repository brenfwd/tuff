import { type PointerEvent, useCallback, useEffect, useRef, useState } from "react";
import type { PanelContext } from "../../../web-ui/app/src/sdk/context";
import {
  type View,
  blockAt,
  bounds,
  levelFor,
  panBy,
  screenOf,
  tileKey,
  visibleTiles,
  zoomAt,
} from "./view";
import { type Area, type Grip, type ShapeKind, MAX_RADIUS, chunkCount, dragTo, grab, handles } from "./shape";

const API = "/plugins/world-map";
const TILES_URL = "/admin/api/v1/plugins/world-map/tiles";
const TILES_TOPIC = "plugin.world-map.tiles";
const MARKERS_TOPIC = "plugin.world-map.markers";
const PREGEN_TOPIC = "plugin.world-map.pregen";
const MAX_FETCHES = 6;
const MAX_BITMAPS = 256;

interface Dimension {
  index: number;
  key: string;
  minY: number;
  height: number;
  ceiling: boolean;
}

interface Info {
  epoch: string;
  tileSize: number;
  maxLevel: number;
  dimensions: Dimension[];
  spawn: { dim: number; x: number; z: number };
}

interface Player {
  uuid: string;
  name: string;
  dim: number;
  x: number;
  y: number;
  z: number;
  yaw: number;
  mode: string;
}

interface Tile {
  bitmap?: ImageBitmap;
  etag?: string;
  loading: boolean;
  stale: boolean;
  used: number;
  retryAt: number;
}

interface Progress {
  id: number;
  dimension: string;
  phase: "removing" | "generating";
  done: number;
  total: number;
  failed: number;
  rate: number;
  eta_secs: number | null;
  cancelling: boolean;
}

interface Finished {
  id: number;
  generated: number;
  total: number;
  failed: number;
  cancelled: boolean;
  error: string | null;
}

interface PregenStatus {
  running: Progress | null;
  last: Finished | null;
}

function describe(status: PregenStatus | null): string {
  const p = status?.running;
  if (p) {
    const verb = p.phase === "removing" ? "Removing" : "Generating";
    const eta = p.eta_secs == null ? "" : `, ETA ${p.eta_secs}s`;
    return `${verb} ${p.done} / ${p.total} chunks (${p.rate.toFixed(1)}/s${eta})${p.cancelling ? ", cancelling" : ""}`;
  }
  const l = status?.last;
  if (!l) return "";
  const how = l.error ? `stopped: ${l.error}` : l.cancelled ? "cancelled" : "finished";
  return `Job ${l.id} ${how}: ${l.generated} of ${l.total} chunks, ${l.failed} failed`;
}

function message(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

function shortKey(key: string): string {
  return key.replace(/^minecraft:/, "");
}

export function MapPage({ ctx }: { ctx: PanelContext }) {
  const canvas = useRef<HTMLCanvasElement>(null);
  const [info, setInfo] = useState<Info | null>(null);
  const [view, setView] = useState<View>({ dim: 0, x: 0, z: 0, scale: 1 });
  const [cursor, setCursor] = useState<{ x: number; z: number } | null>(null);
  const [players, setPlayers] = useState<Player[]>([]);
  type Mark = { id: number; dim: number; type: string; group: string; x: number; z: number; name: string | null };
  const [marks, setMarks] = useState<Mark[]>([]);
  const [shown, setShown] = useState<Record<string, boolean>>({ mob: true, item: false, vehicle: true, other: false });
  const tiles = useRef(new Map<string, Tile>());
  const fetching = useRef(0);
  const viewRef = useRef(view);
  viewRef.current = view;
  const drag = useRef<{ x: number; y: number } | null>(null);
  const [frame, setFrame] = useState(0);
  const [area, setArea] = useState<Area | null>(null);
  const areaRef = useRef(area);
  areaRef.current = area;
  const grip = useRef<Grip | null>(null);
  const [regenerate, setRegenerate] = useState(false);
  const [pregen, setPregen] = useState<PregenStatus | null>(null);
  const redraw = useCallback(() => setFrame((f) => f + 1), []);

  useEffect(() => {
    ctx
      .api<Info>(API)
      .then((i) => {
        setInfo(i);
        setView({ dim: i.spawn.dim, x: i.spawn.x, z: i.spawn.z, scale: 1 });
      })
      .catch((e) => ctx.toast(message(e), "error"));
    ctx
      .api<{ players: Player[] }>(`${API}/markers`)
      .then((m) => setPlayers(m.players))
      .catch(() => {});
    ctx
      .api<PregenStatus>(`${API}/pregen`)
      .then(setPregen)
      .catch(() => {});
  }, [ctx]);

  const load = useCallback(
    (key: string) => {
      const now = performance.now();
      let tile = tiles.current.get(key);
      if (!tile) {
        tile = { loading: false, stale: true, used: now, retryAt: 0 };
        tiles.current.set(key, tile);
      }
      tile.used = now;
      if (tile.loading || !tile.stale || now < tile.retryAt || fetching.current >= MAX_FETCHES) return;
      tile.loading = true;
      fetching.current++;
      const headers: Record<string, string> = {};
      if (tile.etag) headers["If-None-Match"] = tile.etag;
      const done = () => {
        tile.loading = false;
        fetching.current--;
        redraw();
      };
      fetch(`${TILES_URL}/${key}.png`, { headers, credentials: "same-origin", cache: "no-store" })
        .then(async (resp) => {
          if (resp.status === 304) {
            tile.stale = false;
          } else if (resp.ok) {
            const bitmap = await createImageBitmap(await resp.blob());
            tile.bitmap?.close();
            tile.bitmap = bitmap;
            tile.etag = resp.headers.get("etag") ?? undefined;
            tile.stale = false;
          } else {
            const after = Number(resp.headers.get("retry-after") ?? "2");
            tile.retryAt = performance.now() + Math.max(1, after) * 1000;
          }
        })
        .catch(() => {
          tile.retryAt = performance.now() + 2000;
        })
        .finally(done);
    },
    [redraw],
  );

  useEffect(
    () =>
      ctx.events([TILES_TOPIC, MARKERS_TOPIC, PREGEN_TOPIC], ["changed", "players", "entities", "status"], (event, data) => {
        if (event === "status") {
          setPregen(JSON.parse(data) as PregenStatus);
        } else if (event === "players") {
          setPlayers((JSON.parse(data) as { players: Player[] }).players);
        } else if (event === "entities") {
          setMarks((JSON.parse(data) as { entities: Mark[] }).entities);
        } else if (event === "changed") {
          const body = JSON.parse(data) as { tiles?: number[][]; resync?: boolean };
          if (body.resync) {
            for (const tile of tiles.current.values()) tile.stale = true;
          } else {
            for (const [d, l, x, z] of body.tiles ?? []) {
              const tile = tiles.current.get(`${d}/${l}/${x}/${z}`);
              if (tile) tile.stale = true;
            }
          }
          redraw();
        } else {
          for (const tile of tiles.current.values()) tile.stale = true;
          redraw();
        }
      }),
    [ctx, redraw],
  );

  useEffect(() => {
    const el = canvas.current;
    if (!el) return;
    const send = () => {
      const v = viewRef.current;
      const b = bounds(v, el.clientWidth, el.clientHeight);
      ctx
        .api(`${API}/viewport`, {
          method: "POST",
          body: JSON.stringify({
            dim: v.dim,
            level: levelFor(v.scale),
            minX: Math.floor(b.minX),
            minZ: Math.floor(b.minZ),
            maxX: Math.ceil(b.maxX),
            maxZ: Math.ceil(b.maxZ),
          }),
        })
        .catch(() => {});
    };
    const timer = window.setTimeout(send, 400);
    const every = window.setInterval(send, 5000);
    return () => {
      window.clearTimeout(timer);
      window.clearInterval(every);
    };
  }, [ctx, view]);

  useEffect(() => {
    const el = canvas.current;
    if (!el || !info) return;
    const width = el.clientWidth;
    const height = el.clientHeight;
    const ratio = window.devicePixelRatio || 1;
    if (el.width !== Math.round(width * ratio) || el.height !== Math.round(height * ratio)) {
      el.width = Math.round(width * ratio);
      el.height = Math.round(height * ratio);
    }
    const g = el.getContext("2d");
    if (!g) return;
    g.setTransform(ratio, 0, 0, ratio, 0, 0);
    g.imageSmoothingEnabled = false;
    g.clearRect(0, 0, width, height);
    for (const placed of visibleTiles(view, width, height)) {
      const key = tileKey(placed);
      load(key);
      const bitmap = tiles.current.get(key)?.bitmap;
      if (bitmap) g.drawImage(bitmap, placed.left, placed.top, placed.size, placed.size);
    }
    g.font = "12px sans-serif";
    g.textBaseline = "middle";
    for (const p of players) {
      if (p.dim !== view.dim) continue;
      const at = screenOf(view, width, height, p.x, p.z);
      g.fillStyle = p.mode === "spectator" ? "rgba(255,255,255,0.5)" : "#e11d48";
      g.strokeStyle = "#000";
      g.beginPath();
      g.arc(at.left, at.top, 5, 0, Math.PI * 2);
      g.fill();
      g.stroke();
      g.fillStyle = "#fff";
      g.strokeText(p.name, at.left + 8, at.top);
      g.fillText(p.name, at.left + 8, at.top);
    }
    for (const m of marks) {
      if (m.dim !== view.dim || !shown[m.group]) continue;
      const at = screenOf(view, width, height, m.x, m.z);
      g.fillStyle = m.group === "item" ? "#facc15" : m.group === "vehicle" ? "#a78bfa" : "#22c55e";
      g.fillRect(at.left - 2, at.top - 2, 4, 4);
      if (m.name) g.fillText(m.name, at.left + 5, at.top);
    }
    if (area) {
      const centre = screenOf(view, width, height, area.x + 0.5, area.z + 0.5);
      const r = area.radius * view.scale;
      g.strokeStyle = regenerate ? "#f97316" : "#38bdf8";
      g.fillStyle = regenerate ? "rgba(249,115,22,0.15)" : "rgba(56,189,248,0.15)";
      g.lineWidth = 2;
      g.beginPath();
      if (area.shape === "circle") g.arc(centre.left, centre.top, r, 0, Math.PI * 2);
      else g.rect(centre.left - r, centre.top - r, 2 * r, 2 * r);
      g.fill();
      g.stroke();
      g.fillStyle = "#fff";
      for (const h of [centre, ...handles(area, view, width, height)]) {
        g.fillRect(h.left - 4, h.top - 4, 8, 8);
        g.strokeRect(h.left - 4, h.top - 4, 8, 8);
      }
      g.lineWidth = 1;
    }
    if (tiles.current.size > MAX_BITMAPS) {
      const oldest = [...tiles.current.entries()].sort((a, b) => a[1].used - b[1].used);
      for (const [key, tile] of oldest.slice(0, tiles.current.size - MAX_BITMAPS)) {
        tile.bitmap?.close();
        tiles.current.delete(key);
      }
    }
  }, [view, info, players, marks, shown, frame, load, area, regenerate]);

  useEffect(() => {
    const el = canvas.current;
    if (!el) return;
    const observer = new ResizeObserver(redraw);
    observer.observe(el);
    const wheel = (e: WheelEvent) => {
      e.preventDefault();
      const rect = el.getBoundingClientRect();
      const factor = Math.exp(-e.deltaY * 0.002);
      setView((v) => zoomAt(v, rect.width, rect.height, e.clientX - rect.left, e.clientY - rect.top, factor));
    };
    el.addEventListener("wheel", wheel, { passive: false });
    return () => {
      observer.disconnect();
      el.removeEventListener("wheel", wheel);
    };
  }, [redraw]);

  useEffect(
    () => () => {
      for (const tile of tiles.current.values()) tile.bitmap?.close();
      tiles.current.clear();
    },
    [],
  );

  const zoom = (factor: number) => {
    const el = canvas.current;
    if (!el) return;
    setView((v) => zoomAt(v, el.clientWidth, el.clientHeight, el.clientWidth / 2, el.clientHeight / 2, factor));
  };

  const onPointerDown = (e: PointerEvent<HTMLCanvasElement>) => {
    const rect = e.currentTarget.getBoundingClientRect();
    const a = areaRef.current;
    grip.current = a ? grab(a, viewRef.current, rect.width, rect.height, e.clientX - rect.left, e.clientY - rect.top) : null;
    drag.current = { x: e.clientX, y: e.clientY };
    e.currentTarget.setPointerCapture(e.pointerId);
  };
  const onPointerMove = (e: PointerEvent<HTMLCanvasElement>) => {
    const el = e.currentTarget;
    const rect = el.getBoundingClientRect();
    setCursor(blockAt(viewRef.current, rect.width, rect.height, e.clientX - rect.left, e.clientY - rect.top));
    const from = drag.current;
    if (!from) return;
    if (grip.current && areaRef.current) {
      const at = blockAt(viewRef.current, rect.width, rect.height, e.clientX - rect.left, e.clientY - rect.top);
      setArea(dragTo(areaRef.current, grip.current, at.x, at.z));
      return;
    }
    drag.current = { x: e.clientX, y: e.clientY };
    setView((v) => panBy(v, e.clientX - from.x, e.clientY - from.y));
  };
  const onPointerUp = () => {
    drag.current = null;
    grip.current = null;
  };

  const startArea = (shape: ShapeKind) => {
    const v = viewRef.current;
    const el = canvas.current;
    const radius = el ? Math.max(16, Math.round(Math.min(el.clientWidth, el.clientHeight) / 4 / v.scale)) : 64;
    setArea({ x: Math.floor(v.x), z: Math.floor(v.z), radius: Math.min(MAX_RADIUS, radius), shape });
  };

  const generate = () => {
    if (!area) return;
    const count = chunkCount(area);
    if (
      regenerate &&
      !window.confirm(
        `Delete and regenerate ${count} chunks? Everything built in them, with their entities and block entities, is lost.`,
      )
    )
      return;
    ctx
      .api<Progress>(`${API}/pregen`, {
        method: "POST",
        body: JSON.stringify({
          dim: view.dim,
          x: area.x,
          z: area.z,
          radius: area.radius,
          shape: area.shape,
          regenerate,
          ...(regenerate ? { confirm: "regenerate" } : {}),
        }),
      })
      .then((p) => {
        setPregen((s) => ({ running: p, last: s?.last ?? null }));
        ctx.toast(`Generating ${p.total} chunks`, "info");
      })
      .catch((e) => ctx.toast(message(e), "error"));
  };

  const cancel = () => {
    ctx
      .api(`${API}/pregen/cancel`, { method: "POST", body: "{}" })
      .catch((e) => ctx.toast(message(e), "error"));
  };

  return (
    <section className="flex h-full min-h-[480px] flex-col gap-2 p-4">
      <div className="flex flex-wrap items-center gap-2 text-sm">
        <h1 className="mr-2 text-lg font-semibold">World map</h1>
        <select
          className="rounded border px-2 py-1"
          value={view.dim}
          onChange={(e) => setView((v) => ({ ...v, dim: Number(e.target.value) }))}
        >
          {(info?.dimensions ?? []).map((d) => (
            <option key={d.index} value={d.index}>
              {shortKey(d.key)}
            </option>
          ))}
        </select>
        {Object.keys(shown).map((g) => (
          <label className="flex items-center gap-1" key={g}>
            <input checked={shown[g]} onChange={(e) => setShown({ ...shown, [g]: e.target.checked })} type="checkbox" />
            {g}
          </label>
        ))}
        <button className="rounded border px-2 py-1" onClick={() => zoom(2)} type="button">
          +
        </button>
        <button className="rounded border px-2 py-1" onClick={() => zoom(0.5)} type="button">
          -
        </button>
        <button
          className="rounded border px-2 py-1"
          type="button"
          onClick={() =>
            info && setView((v) => ({ ...v, dim: info.spawn.dim, x: info.spawn.x, z: info.spawn.z }))
          }
        >
          Spawn
        </button>
        <span className="tabular-nums opacity-70">
          {cursor ? `x ${cursor.x}, z ${cursor.z}` : ""} (zoom {view.scale >= 1 ? `${view.scale.toFixed(1)}x` : `1:${(1 / view.scale).toFixed(0)}`})
        </span>
      </div>
      <div className="flex flex-wrap items-center gap-2 text-sm">
        <span className="font-medium">Pre-generate</span>
        <select
          className="rounded border px-2 py-1"
          value={area?.shape ?? ""}
          onChange={(e) => {
            const shape = e.target.value as ShapeKind | "";
            if (!shape) setArea(null);
            else if (area) setArea({ ...area, shape });
            else startArea(shape);
          }}
        >
          <option value="">no shape</option>
          <option value="circle">circle</option>
          <option value="square">square</option>
        </select>
        {area && (
          <>
            <label className="flex items-center gap-1">
              radius
              <input
                className="w-24 rounded border px-2 py-1 tabular-nums"
                type="number"
                min={0}
                max={MAX_RADIUS}
                value={area.radius}
                onChange={(e) =>
                  setArea({ ...area, radius: Math.min(MAX_RADIUS, Math.max(0, Math.round(Number(e.target.value) || 0))) })
                }
              />
              blocks
            </label>
            <span className="tabular-nums opacity-70">
              at {area.x}, {area.z}: {chunkCount(area)} chunks
            </span>
            <label className="flex items-center gap-1">
              <input type="checkbox" checked={regenerate} onChange={(e) => setRegenerate(e.target.checked)} />
              force regenerate
            </label>
            <button
              className="rounded border px-2 py-1"
              type="button"
              disabled={!!pregen?.running}
              onClick={generate}
            >
              Generate
            </button>
          </>
        )}
        {pregen?.running && (
          <button className="rounded border px-2 py-1" type="button" onClick={cancel}>
            Cancel
          </button>
        )}
        <span className="tabular-nums opacity-70">{describe(pregen)}</span>
      </div>
      <canvas
        ref={canvas}
        className="min-h-[420px] w-full flex-1 cursor-grab touch-none rounded bg-neutral-800"
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={onPointerUp}
        onPointerLeave={() => setCursor(null)}
      />
    </section>
  );
}
