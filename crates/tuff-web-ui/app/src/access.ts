export interface AuditEntry {
  ts: number;
  principal: string;
  remote: string;
  action: string;
  target: string;
  detail: string;
  outcome: string;
}

export interface AuditFilter {
  principal: string;
  action: string;
}

/** The audit API query for one page of `filter` before `before`. */
export function auditQuery(filter: AuditFilter, limit: number, before?: number): string {
  const params = new URLSearchParams({ limit: String(limit) });
  if (before !== undefined) params.set("before", String(before));
  if (filter.principal.trim()) params.set("principal", filter.principal.trim());
  if (filter.action.trim()) params.set("action", filter.action.trim());
  return `/audit?${params.toString()}`;
}

/** Whether a live entry belongs in a list shown with `filter`, as the
 * server filters: principal in any case, action exact or by a prefix
 * ending in a dot. */
export function auditMatches(entry: AuditEntry, filter: AuditFilter): boolean {
  const p = filter.principal.trim();
  const a = filter.action.trim();
  if (p && entry.principal.toLowerCase() !== p.toLowerCase()) return false;
  if (a && entry.action !== a && !(a.endsWith(".") && entry.action.startsWith(a))) return false;
  return true;
}

/** Adds a live audit event's entry on top when it matches. */
export function prependLive(entries: AuditEntry[], data: string, filter: AuditFilter, max = 1000): AuditEntry[] {
  let entry: AuditEntry;
  try {
    entry = JSON.parse(data);
  } catch {
    return entries;
  }
  if (!entry || typeof entry.action !== "string" || !auditMatches(entry, filter)) return entries;
  return [entry, ...entries].slice(0, max);
}

/** A short "3m ago" style age of a unix-millisecond time. */
export function age(ms: number, now = Date.now()): string {
  const s = Math.max(0, Math.round((now - ms) / 1000));
  if (s < 60) return `${s}s`;
  if (s < 3600) return `${Math.floor(s / 60)}m`;
  if (s < 86400) return `${Math.floor(s / 3600)}h`;
  return `${Math.floor(s / 86400)}d`;
}

/** Token lifetimes offered when creating one, in seconds (null: never). */
export const TOKEN_EXPIRIES: { label: string; seconds: number | null }[] = [
  { label: "never expires", seconds: null },
  { label: "1 day", seconds: 86_400 },
  { label: "7 days", seconds: 7 * 86_400 },
  { label: "30 days", seconds: 30 * 86_400 },
  { label: "90 days", seconds: 90 * 86_400 },
  { label: "1 year", seconds: 365 * 86_400 },
];

/** How a token's expiry reads in the list. */
export function expiryLabel(expires: number | null, now = Date.now()): string {
  if (expires === null) return "never";
  if (expires <= now) return "expired";
  return `in ${age(now, expires)}`;
}

export function validTokenName(name: string): boolean {
  return /^[A-Za-z0-9_.-]{1,64}$/.test(name);
}

/** The time of day (0 to 23999) of a clock's total ticks. */
export function dayTime(ticks: number | null): number | null {
  return ticks === null ? null : ((ticks % 24000) + 24000) % 24000;
}

export interface Border {
  center: [number, number];
  size: number;
  target: number;
  lerpTicks: number;
  damagePerBlock: number;
  safeZone: number;
  warningBlocks: number;
  warningTicks: number;
}

const round = (value: number, digits: number) => Number(value.toFixed(digits)).toString();

/** A dimension's border as the World page lists it, with any size change under way. */
export function borderLabel(border: Border): string {
  const at = `${round(border.size, 1)} wide at ${border.center.map((c) => round(c, 2)).join(", ")}`;
  if (border.lerpTicks <= 0) return at;
  const verb = border.target < border.size ? "shrinking" : "growing";
  return `${at}, ${verb} to ${round(border.target, 1)} over ${round(border.lerpTicks / 20, 1)}s`;
}

/** The border's damage and warning settings. */
export function borderRules(border: Border): string {
  return `${round(border.damagePerBlock, 2)} damage per block past ${round(border.safeZone, 2)}; warns at ${border.warningBlocks} blocks or ${round(border.warningTicks / 20, 2)}s`;
}
