export interface BackupInfo {
  name: string;
  size: number;
  created_unix: number;
}

export interface Progress {
  phase: string;
  files_done: number;
  files_total: number;
  bytes_done: number;
  bytes_total: number;
}

export interface Running {
  id: number;
  name: string;
  by: string;
  started_unix: number;
  progress: Progress;
}

export interface Finished {
  id: number;
  name: string;
  by: string;
  error: string | null;
  size: number;
  hold_ms: number;
  duration_ms: number;
  pruned: string[];
}

export interface Staged {
  backup: string;
  by: string;
  staged_unix: number;
  kind: "folder" | "server_file";
}

export interface BackupsState {
  level: string;
  world: "folder" | "server_file" | null;
  backups: BackupInfo[];
  status: { running: Running | null; last: Finished | null };
  staged: Staged | null;
  maxUpload: number;
}

const UNITS = ["B", "KiB", "MiB", "GiB", "TiB"];

export function formatBytes(bytes: number): string {
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < UNITS.length - 1) {
    value /= 1024;
    unit++;
  }
  return unit === 0 ? `${value} B` : `${value.toFixed(value >= 100 ? 0 : 1)} ${UNITS[unit]}`;
}

export function percent(done: number, total: number): number {
  if (total <= 0) return 0;
  return Math.max(0, Math.min(100, Math.floor((done / total) * 100)));
}

const PHASES: Record<string, string> = {
  saving: "Saving the world",
  copying: "Copying",
  compressing: "Compressing",
  verifying: "Verifying",
  finishing: "Finishing",
};

export function phaseLabel(phase: string): string {
  return PHASES[phase] ?? phase;
}

/** Which backups a world of `world` can be restored from. */
export function restorable(world: BackupsState["world"], name: string): boolean {
  if (world === "folder") return name.endsWith(".zip");
  if (world === "server_file") return name.endsWith(".mcserver");
  return false;
}

/** Why an upload of `size` bytes cannot start, if it cannot. */
export function uploadProblem(size: number, max: number): string | null {
  if (size <= 0) return "The file is empty.";
  if (size > max) return `The file is ${formatBytes(size)}; the limit is ${formatBytes(max)}.`;
  return null;
}

export function confirmed(typed: string, level: string): boolean {
  return typed.trim() === level;
}

export function errorText(status: number, body: string): string {
  try {
    const data = JSON.parse(body);
    if (data && typeof data.error === "string") return data.error;
  } catch {
    /* not json */
  }
  return status ? `HTTP ${status}` : "the connection failed";
}
