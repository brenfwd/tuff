import type { TextFormat } from "./MiniMessageField";
import { validIp } from "./players";

export interface Setting {
  name: string;
  kind: "string" | "int" | "bool" | "url" | "ip" | "port";
  description: string;
  hints: string[];
  runtimeOnly: boolean;
  feature: string;
  group: string;
  secret: boolean;
  textComponent?: boolean;
  lockout: boolean;
  value?: string;
  default?: string;
  set?: boolean;
}

export interface Property {
  name: string;
  known: boolean;
  present: boolean;
  effect: "live" | "restart" | "creation" | "unsupported" | "permanent" | "saved";
  /** What setting the key does, e.g. "needs a restart: ...". */
  effectNote: string;
  /** Why a restart-only or unsupported key is so. */
  reason?: string;
  /** Returned by a save: what the change did beyond being saved. */
  note?: string;
  secret: boolean;
  kind: "bool" | "int" | "choice" | "string";
  min?: number;
  max?: number;
  options?: string[];
  value?: string;
  default?: string | null;
  set?: boolean;
}

export interface GameRule {
  name: string;
  kind: "bool" | "int";
  default: string;
  min?: number;
  max?: number;
  value: string | null;
}

export interface ConfigState {
  settings: Setting[];
  properties: Property[];
  gameRules: GameRule[];
  writable: boolean;
}

export interface Bounds {
  min?: number;
  max?: number;
  options?: string[];
}

export const MAX_VALUE = 4096;

/** Why `text` is not a valid value of `kind`, or null when it is. The
 * server checks again with the key's own validator. */
export function invalid(kind: string, text: string, bounds: Bounds = {}): string | null {
  const t = text.trim();
  if (new TextEncoder().encode(text).length > MAX_VALUE) return `At most ${MAX_VALUE} bytes.`;
  if (/[\u0000-\u001f\u007f-\u009f]/.test(text)) return "No line breaks or control characters.";
  switch (kind) {
    case "bool":
      return t === "true" || t === "false" ? null : "Must be true or false.";
    case "int": {
      if (!/^-?\d+$/.test(t)) return "Must be a whole number.";
      const n = Number(t);
      if (bounds.min !== undefined && n < bounds.min) return `At least ${bounds.min}.`;
      if (bounds.max !== undefined && n > bounds.max) return `At most ${bounds.max}.`;
      return null;
    }
    case "port":
      return /^\d+$/.test(t) && Number(t) <= 65535 ? null : "Must be a port from 0 to 65535.";
    case "ip":
      return validIp(t) ? null : "Must be an IP address.";
    case "url":
      return /^[A-Za-z][A-Za-z0-9+.-]*:\/\/\S+$/.test(t) ? null : "Must be a url.";
    case "choice":
      return bounds.options?.includes(t.toLowerCase()) ? null : `One of ${bounds.options?.join(", ")}.`;
    default:
      return null;
  }
}

/** Whether an entry matches a search: every word appears in its name,
 * description, group or feature. */
export function matches(entry: { name: string; description?: string; group?: string; feature?: string }, query: string): boolean {
  const hay = [entry.name, entry.description ?? "", entry.group ?? "", entry.feature ?? ""].join(" ").toLowerCase();
  return query
    .toLowerCase()
    .split(/\s+/)
    .filter(Boolean)
    .every((word) => hay.includes(word));
}

/** Settings by group, groups and keys sorted by name. */
export function groupSettings(settings: Setting[]): [string, Setting[]][] {
  const groups = new Map<string, Setting[]>();
  for (const s of settings) {
    const list = groups.get(s.group) ?? [];
    list.push(s);
    groups.set(s.group, list);
  }
  return [...groups.entries()]
    .sort(([a], [b]) => a.localeCompare(b))
    .map(([g, list]) => [g, list.sort((a, b) => a.name.localeCompare(b.name))]);
}

/** Whether a non-secret entry holds its default. */
export function isDefault(entry: { value?: string | null; default?: string | null; secret?: boolean; set?: boolean }): boolean {
  if (entry.secret) return !entry.set;
  return entry.default === undefined || entry.default === null || entry.value === entry.default;
}

/** The toast after saving a property, from the server's answer. */
export function savedMessage(p: Property): string {
  if (p.note) return `${p.name}: ${p.note}`;
  return p.effect === "live" ? `Saved ${p.name}.` : `Saved ${p.name} (${p.effectNote}).`;
}

/** Replaces the entry named like `entry` in `list`. */
export function replaceEntry<T extends { name: string }>(list: T[], entry: T): T[] {
  return list.map((e) => (e.name === entry.name ? entry : e));
}

export interface TreeNode<T> {
  segment: string;
  path: string;
  entry?: T;
  children: TreeNode<T>[];
  count: number;
}

/** Entries as a tree split on '.', children sorted by segment; `count` is
 * the number of entries at or below a node. */
export function buildTree<T extends { name: string }>(entries: T[]): TreeNode<T>[] {
  const root: TreeNode<T> = { segment: "", path: "", children: [], count: 0 };
  for (const entry of entries) {
    let node = root;
    let path = "";
    for (const segment of entry.name.split(".")) {
      path = path ? `${path}.${segment}` : segment;
      let next = node.children.find((c) => c.segment === segment);
      if (!next) {
        next = { segment, path, children: [], count: 0 };
        node.children.push(next);
      }
      node = next;
    }
    node.entry = entry;
  }
  const finish = (node: TreeNode<T>): number => {
    node.children.sort((a, b) => a.segment.localeCompare(b.segment));
    node.count = (node.entry ? 1 : 0) + node.children.reduce((n, c) => n + finish(c), 0);
    return node.count;
  };
  finish(root);
  return root.children;
}

/** The text format an entry's value is written in when it is a text component, else null. */
export function textFormat(entry: Setting | Property): TextFormat | null {
  if ("lockout" in entry) return entry.textComponent && !entry.secret ? "minimessage" : null;
  return entry.name === "motd" ? "section" : null;
}
