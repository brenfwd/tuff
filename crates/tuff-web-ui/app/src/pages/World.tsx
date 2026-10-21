import { useCallback, useEffect, useState } from "react";
import { api } from "../api";
import { Toast } from "../plugins";
import { borderLabel, borderRules, dayTime, type Border } from "../access";

interface Overview {
  backend: string;
  seed: string | null;
  spawn: { dimension: string; position: [number, number, number] };
  gameTime: number;
  difficulty: string;
  weather: { raining: boolean; thundering: boolean };
  dimensions: {
    key: string;
    dayTime: number | null;
    loaded: { regions: number; chunks: number; entities: number; players: number };
    border: Border;
  }[];
  loaded: { regions: number; chunks: number; entities: number; players: number };
  datapacks: { available: number; selected: string[] };
  backups: { count: number; latest: { name: string; size: number; created: number } | null } | null;
  canSave: boolean;
}

function Stat({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="rounded border border-zinc-200 p-3 dark:border-zinc-800">
      <div className="text-xs text-zinc-500">{label}</div>
      <div className="font-mono text-sm break-all">{children}</div>
    </div>
  );
}

export function World({ toast }: { toast: Toast }) {
  const [world, setWorld] = useState<Overview | null>(null);
  const [busy, setBusy] = useState(false);

  const refresh = useCallback(async () => {
    try {
      setWorld(await api<Overview>("/world"));
    } catch (e) {
      toast(e instanceof Error ? e.message : String(e), "error");
    }
  }, [toast]);

  useEffect(() => {
    void refresh();
    const timer = setInterval(() => void refresh(), 10000);
    return () => clearInterval(timer);
  }, [refresh]);

  const save = async () => {
    setBusy(true);
    try {
      const r = await api<{ ok: boolean; feedback: string[] }>("/world/save", { method: "POST", body: "{}" });
      toast(r.feedback.join(" ") || "Saved.", r.ok ? "info" : "error");
    } catch (e) {
      toast(e instanceof Error ? e.message : String(e), "error");
    } finally {
      setBusy(false);
    }
  };

  if (!world) return <p className="p-4 text-sm text-zinc-500">Loading world...</p>;
  const weather = world.weather.thundering ? "thunder" : world.weather.raining ? "rain" : "clear";
  return (
    <div className="flex flex-col gap-4 p-4">
      <div className="grid grid-cols-2 gap-2 sm:grid-cols-4">
        <Stat label="Backend">{world.backend}</Stat>
        {world.seed !== null && <Stat label="Seed">{world.seed}</Stat>}
        <Stat label="Spawn">
          {world.spawn.dimension} {world.spawn.position.join(", ")}
        </Stat>
        <Stat label="Game time">{world.gameTime}</Stat>
        <Stat label="Difficulty">{world.difficulty}</Stat>
        <Stat label="Weather">{weather}</Stat>
        <Stat label="Loaded">
          {world.loaded.regions} regions, {world.loaded.chunks} chunks, {world.loaded.entities} entities
        </Stat>
        <Stat label="Data packs">
          {world.datapacks.selected.length} of {world.datapacks.available} on
        </Stat>
        <Stat label="Backups">
          {world.backups ? `${world.backups.count}${world.backups.latest ? `, latest ${new Date(world.backups.latest.created * 1000).toLocaleString()}` : ""}` : "unavailable"}
        </Stat>
      </div>
      <table className="w-full text-left text-sm">
        <tbody>
          <tr className="text-zinc-500">
            <th className="py-1">Dimension</th>
            <th>Time of day</th>
            <th>Regions</th>
            <th>Chunks</th>
            <th>Entities</th>
            <th>Players</th>
            <th>Border</th>
          </tr>
          {world.dimensions.map((d) => (
            <tr key={d.key} className="border-t border-zinc-200 dark:border-zinc-800">
              <td className="py-1 font-mono">{d.key}</td>
              <td>{dayTime(d.dayTime) ?? ""}</td>
              <td>{d.loaded.regions}</td>
              <td>{d.loaded.chunks}</td>
              <td>{d.loaded.entities}</td>
              <td>{d.loaded.players}</td>
              <td title={borderRules(d.border)}>{borderLabel(d.border)}</td>
            </tr>
          ))}
        </tbody>
      </table>
      {world.canSave && (
        <div>
          <button
            type="button"
            className="rounded border border-zinc-300 px-2 py-1 text-sm hover:bg-zinc-100 disabled:opacity-40 dark:border-zinc-700 dark:hover:bg-zinc-900"
            disabled={busy}
            onClick={() => void save()}
          >
            Save now
          </button>
        </div>
      )}
    </div>
  );
}
