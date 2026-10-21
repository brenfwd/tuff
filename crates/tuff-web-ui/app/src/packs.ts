import { formatBytes } from "./backups";

export type Compatibility = "compatible" | "too_old" | "too_new" | "unknown";
export type FormatPair = [number, number] | null;

export interface DataPackMeta {
  description: string;
  formats: [FormatPair, FormatPair];
  compatibility: Compatibility;
  features: string[];
  missingFeatures: string[];
}

export interface DataPack {
  id: string;
  name: string;
  title: string;
  source: "builtin" | "world";
  kind: "zip" | "folder" | null;
  required: boolean;
  selected: boolean;
  position: number | null;
  meta: DataPackMeta | null;
  error: string | null;
}

export interface DataPacksState {
  packs: DataPack[];
  selected: string[];
  maxUpload: number;
}

export interface ResourcePack {
  id: string;
  name: string;
  kind: "custom" | "uploaded" | "feature";
  required: boolean;
  prompt: string | null;
  offerOnJoin: boolean;
  order: number | null;
  essential: boolean;
  state: "building" | "ready" | "failed";
  sha1: string | null;
  size: number | null;
  holders: number;
  editable: boolean;
  url: string;
  description: string | null;
  formats: [[number, number], [number, number]] | null;
  compatibility: Compatibility | null;
  uploadedBy: string | null;
}

export interface PlayerPack {
  id: string;
  status: string | null;
  loaded: boolean;
  current: boolean;
}

export interface ResourcePacksState {
  packs: ResourcePack[];
  players: { uuid: string; name: string; packs: PlayerPack[] }[];
  enabled: boolean;
  maxUpload: number;
  fileArea: boolean;
  orders: [number, number];
}

/** Enabled packs first, highest priority on top, then the rest by name. */
export function sortDataPacks(packs: DataPack[]): DataPack[] {
  return [...packs].sort((a, b) => {
    if (a.position !== null && b.position !== null) return b.position - a.position;
    if (a.position !== null) return -1;
    if (b.position !== null) return 1;
    return a.name.localeCompare(b.name);
  });
}

function pair(p: FormatPair | [number, number]): string {
  if (!p) return "?";
  return p[1] === 0 || p[1] === 2147483647 ? `${p[0]}` : `${p[0]}.${p[1]}`;
}

export function formatRange(range: [FormatPair, FormatPair] | null | undefined): string {
  if (!range || !range[0] || !range[1]) return "unknown";
  const [lo, hi] = [pair(range[0]), pair(range[1])];
  return lo === hi ? lo : `${lo}-${hi}`;
}

const COMPAT: Record<Compatibility, string> = {
  compatible: "compatible",
  too_old: "made for an older version",
  too_new: "made for a newer version",
  unknown: "unknown format",
};

export function compatibilityLabel(c: Compatibility | null | undefined): string {
  return c ? COMPAT[c] : "";
}

const STATUS: Record<string, string> = {
  loaded: "Loaded",
  declined: "Declined",
  failed_download: "Download failed",
  accepted: "Accepted",
  downloaded: "Downloaded",
  invalid_url: "Bad URL",
  failed_reload: "Reload failed",
  discarded: "Discarded",
};

export function statusLabel(p: PlayerPack): string {
  const base = p.status ? (STATUS[p.status] ?? p.status) : "Offered";
  return p.current || !p.loaded ? base : `${base} (older version)`;
}

/** Why a pack upload of `size` bytes cannot start, if it cannot. */
export function packUploadProblem(name: string, size: number, max: number): string | null {
  if (!name.toLowerCase().endsWith(".zip")) return "Packs are uploaded as .zip files.";
  if (size <= 0) return "The file is empty.";
  if (size > max) return `The file is ${formatBytes(size)}; the limit is ${formatBytes(max)}.`;
  return null;
}

export function kindLabel(kind: ResourcePack["kind"]): string {
  return kind === "custom" ? "Custom content" : kind === "uploaded" ? "Uploaded" : "Feature";
}

/** PUTs `file` to `path` (under the API) with progress, resolving the JSON answer. */
export function uploadFile(path: string, file: File, csrf: string, onProgress: (p: number) => void): Promise<unknown> {
  return new Promise((resolve, reject) => {
    const xhr = new XMLHttpRequest();
    xhr.open("PUT", `/admin/api/v1${path}`);
    xhr.setRequestHeader("x-tuff-csrf", csrf);
    xhr.upload.onprogress = (e) => e.lengthComputable && onProgress(Math.floor((e.loaded / Math.max(1, e.total)) * 100));
    xhr.onload = () => {
      let data: unknown = null;
      try {
        data = JSON.parse(xhr.responseText);
      } catch {
        data = null;
      }
      if (xhr.status === 200) resolve(data);
      else {
        const err = data && typeof data === "object" && "error" in data ? String((data as { error: unknown }).error) : xhr.status ? `HTTP ${xhr.status}` : "the connection failed";
        reject(new Error(err));
      }
    };
    xhr.onerror = () => reject(new Error("the connection failed"));
    xhr.send(file);
  });
}
