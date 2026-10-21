export const API = "/admin/api/v1";

export interface Session {
  principal: { kind: "console" | "operator" | "token"; name: string };
  level: number;
  csrf: string;
  scopes: string[];
}

let csrf = "";

export function setCsrf(token: string) {
  csrf = token;
}

export class ApiError extends Error {
  constructor(
    public status: number,
    message: string,
  ) {
    super(message);
  }
}

export async function api<T>(path: string, init: RequestInit = {}): Promise<T> {
  const headers = new Headers(init.headers);
  if (init.body !== undefined) headers.set("content-type", "application/json");
  if (init.method && init.method !== "GET") headers.set("x-tuff-csrf", csrf);
  const resp = await fetch(API + path, { ...init, headers, credentials: "same-origin" });
  const text = await resp.text();
  let data: unknown = null;
  try {
    data = text ? JSON.parse(text) : null;
  } catch {
    data = null;
  }
  if (!resp.ok) {
    const message =
      data && typeof data === "object" && "error" in data
        ? String((data as { error: unknown }).error)
        : resp.statusText || `HTTP ${resp.status}`;
    throw new ApiError(resp.status, message);
  }
  return data as T;
}

export function storageGet(key: string): string | null {
  try {
    return window.localStorage.getItem(key);
  } catch {
    return null;
  }
}

export function storageSet(key: string, value: string) {
  try {
    window.localStorage.setItem(key, value);
  } catch {
    /* storage unavailable */
  }
}
