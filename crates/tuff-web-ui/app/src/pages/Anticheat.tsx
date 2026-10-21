import { useEffect, useState } from "react";
import { api } from "../api";

interface CheckRow {
  check: string;
  flags: number;
  kicks: number;
  lastHour: number;
  lastDay: number;
}

export function Anticheat() {
  const [rows, setRows] = useState<CheckRow[] | null | undefined>(undefined);

  useEffect(() => {
    const load = async () => {
      try {
        const r = await api<{ checks?: CheckRow[] }>("/plugins/anticheat/summary");
        setRows(r.checks ?? []);
      } catch {
        setRows(null);
      }
    };
    void load();
    const timer = setInterval(() => void load(), 15000);
    return () => clearInterval(timer);
  }, []);

  if (rows === undefined) return <p className="p-4 text-sm text-zinc-500">Loading...</p>;
  if (rows === null) return <p className="p-4 text-sm text-zinc-500">Anti-cheat alerts are not available to you.</p>;
  const sorted = [...rows].sort((a, b) => b.lastDay - a.lastDay || b.flags - a.flags);
  return (
    <div className="p-4">
      <h2 className="mb-2 font-semibold">Alerts by check</h2>
      {sorted.length === 0 ? (
        <p className="text-sm text-zinc-500">No alerts since the server started.</p>
      ) : (
        <table className="text-sm">
          <thead>
            <tr className="text-left text-zinc-500">
              <th className="pr-6">Check</th>
              <th className="pr-6">Last hour</th>
              <th className="pr-6">Last day</th>
              <th className="pr-6">Total</th>
              <th>Kicks</th>
            </tr>
          </thead>
          <tbody>
            {sorted.map((r) => (
              <tr key={r.check}>
                <td className="pr-6">{r.check}</td>
                <td className="pr-6">{r.lastHour}</td>
                <td className="pr-6">{r.lastDay}</td>
                <td className="pr-6">{r.flags}</td>
                <td>{r.kicks}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </div>
  );
}
