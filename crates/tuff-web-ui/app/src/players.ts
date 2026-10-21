export interface OnlinePlayer {
  uuid: string;
  name: string;
  ping: number;
  gameMode: string;
  dimension: string | null;
  position: [number, number, number] | null;
  opLevel: number;
  brand: string | null;
  pack: string | null;
  address?: string;
}

export interface OfflinePlayer {
  uuid: string;
  name: string;
  opLevel: number;
  whitelisted: boolean;
  banned: boolean;
}

export interface Ban {
  uuid?: string;
  name?: string;
  ip?: string;
  source: string;
  created: number;
  expires: number | null;
  reason: string | null;
}

export interface PlayersState {
  online: OnlinePlayer[];
  offline: OfflinePlayer[];
  ops: { uuid: string; name: string; level: number; bypassesPlayerLimit: boolean }[];
  whitelist: { enabled: boolean; enforced: boolean; entries: { uuid: string; name: string }[] };
  bans: Ban[];
  ipBans: Ban[] | null;
  moderate: boolean;
  level: number;
}

export interface Violations {
  uuid: string;
  name: string;
  flags: number;
  kicks: number;
  checks: { check: string; flags: number; kicks: number; level: number; agoMs: number }[];
}

export const GAME_MODES = ["survival", "creative", "adventure", "spectator"];

export function validName(name: string): boolean {
  return /^[A-Za-z0-9_]{1,16}$/.test(name);
}

export function validText(text: string): boolean {
  return text.trim().length > 0 && [...text].length <= 256 && !/[\u0000-\u001f\u007f-\u009f]/.test(text);
}

export function validIp(ip: string): boolean {
  if (/^(\d{1,3})(\.\d{1,3}){3}$/.test(ip)) return ip.split(".").every((p) => Number(p) <= 255);
  return ip.includes(":") && /^[0-9A-Fa-f:.]+$/.test(ip);
}

/** Applies one `players` topic event to the online list. */
export function applyEvent(online: OnlinePlayer[], event: string, data: string): OnlinePlayer[] {
  let row: Partial<OnlinePlayer> & { uuid: string };
  try {
    row = JSON.parse(data);
  } catch {
    return online;
  }
  if (!row || typeof row.uuid !== "string") return online;
  if (event === "leave") return online.filter((p) => p.uuid !== row.uuid);
  const index = online.findIndex((p) => p.uuid === row.uuid);
  if (index < 0) return [...online, row as OnlinePlayer].sort((a, b) => a.name.localeCompare(b.name));
  const next = online.slice();
  next[index] = { ...online[index], ...row };
  return next;
}

export function formatPosition(p: OnlinePlayer["position"]): string {
  return p ? p.map((v) => v.toFixed(1)).join(", ") : "";
}

export function packLabel(pack: string | null): string {
  if (!pack) return "none";
  return pack.replace(/_/g, " ");
}
