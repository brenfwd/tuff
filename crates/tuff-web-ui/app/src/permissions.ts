export interface PermNode {
  id: string;
  description: string;
  default: boolean;
  op_bypass: boolean;
  op_level: number;
}

export interface PermPlayer {
  uuid: string;
  name: string;
  roles: string[];
  nodes: Record<string, boolean>;
}

export type AttributeKey = "prefix" | "suffix" | "name_color" | "display_name";

export const ATTRIBUTES: { key: AttributeKey; label: string }[] = [
  { key: "display_name", label: "Display name" },
  { key: "prefix", label: "Prefix" },
  { key: "suffix", label: "Suffix" },
  { key: "name_color", label: "Name colour" },
];

export interface PermRole {
  name: string;
  priority: number;
  members: string[];
  nodes: Record<string, boolean>;
  attributes: Partial<Record<AttributeKey, string>>;
}

export interface PermState {
  nodes: PermNode[];
  players: PermPlayer[];
  roles: PermRole[];
  default_role: string;
}

export type Held = boolean | null;

export type PermChange =
  | { kind: "player"; player: string; node: string; held: Held }
  | { kind: "role"; role: string; node: string; held: Held }
  | { kind: "create_role"; role: string; priority: number }
  | { kind: "delete_role"; role: string }
  | { kind: "priority"; role: string; priority: number }
  | { kind: "member"; player: string; role: string; member: boolean }
  | { kind: "attribute"; role: string; attribute: AttributeKey; value: string | null };

const ROLE_NAME = /^[a-z0-9_-]{1,32}$/;
const SEGMENT = /^[a-z0-9_-]+$/;

export function validRole(name: string): boolean {
  return ROLE_NAME.test(name);
}

/** A node id, `*`, or segments ending in `.*`, as the server accepts. */
export function validPattern(pattern: string): boolean {
  if (pattern === "*") return true;
  if (pattern.length > 128) return false;
  if (pattern.endsWith(".*")) return pattern.slice(0, -2).split(".").every((s) => SEGMENT.test(s));
  const parts = pattern.split(".");
  return parts.length >= 2 && parts.every((s) => SEGMENT.test(s));
}

/** Nodes whose id or description contains every word of `query`. */
export function filterNodes(nodes: PermNode[], query: string): PermNode[] {
  const words = query.toLowerCase().split(/\s+/).filter(Boolean);
  if (words.length === 0) return nodes;
  return nodes.filter((n) => {
    const text = `${n.id} ${n.description}`.toLowerCase();
    return words.every((w) => text.includes(w));
  });
}

/** Who holds a node without any setting. */
export function fallbackLabel(node: PermNode): string {
  if (node.default) return "everyone";
  if (!node.op_bypass) return "nobody (no operator bypass)";
  return `operators level ${node.op_level}+`;
}

export function heldLabel(held: boolean): string {
  return held ? "granted" : "denied";
}

/** A setting's entries in id order. */
export function entries(nodes: Record<string, boolean>): [string, boolean][] {
  return Object.entries(nodes).sort(([a], [b]) => a.localeCompare(b));
}

/** The patterns that can decide `id`, most specific first: the id itself,
 * then each parent's wildcard, then `*`. A wildcard row `a.b.*` starts at
 * itself. */
export function candidates(id: string): string[] {
  const out: string[] = [];
  let parts = id.split(".");
  if (id.endsWith(".*")) {
    out.push(id);
    parts = parts.slice(0, -1);
  } else if (id !== "*") {
    out.push(id);
  }
  for (let i = parts.length - 1; i >= 1; i--) out.push(`${parts.slice(0, i).join(".")}.*`);
  out.push("*");
  return [...new Set(out)];
}

/** The setting that decides `id` in `nodes`: the explicit one when set,
 * else the nearest wildcard, with the pattern it came from. */
export function effective(nodes: Record<string, boolean>, id: string): { held: boolean; from: string } | null {
  for (const pattern of candidates(id)) {
    if (pattern in nodes) return { held: nodes[pattern], from: pattern };
  }
  return null;
}

export interface NodeBranch {
  segment: string;
  /** Dotted path; a branch's wildcard pattern is `${path}.*`. */
  path: string;
  node?: PermNode;
  children: NodeBranch[];
  count: number;
}

/** Registered nodes as a tree split on '.', children sorted, keeping only
 * nodes matching `query` (and their ancestors). */
export function nodeTree(nodes: PermNode[], query = ""): NodeBranch[] {
  const root: NodeBranch = { segment: "", path: "", children: [], count: 0 };
  for (const node of filterNodes(nodes, query)) {
    let at = root;
    for (const segment of node.id.split(".")) {
      const path = at.path ? `${at.path}.${segment}` : segment;
      let next = at.children.find((c) => c.segment === segment);
      if (!next) {
        next = { segment, path, children: [], count: 0 };
        at.children.push(next);
      }
      at = next;
    }
    at.node = node;
  }
  const finish = (b: NodeBranch): number => {
    b.children.sort((x, y) => x.segment.localeCompare(y.segment));
    b.count = (b.node ? 1 : 0) + b.children.reduce((n, c) => n + finish(c), 0);
    return b.count;
  };
  finish(root);
  return root.children;
}
