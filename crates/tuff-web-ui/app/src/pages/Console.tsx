import { KeyboardEvent, useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { api, Session, storageGet, storageSet } from "../api";
import { subscribe } from "../events";
import { useStickToBottom } from "../useStickToBottom";

interface Line {
  seq: number;
  ts: number;
  level: string;
  target: string;
  text: string;
}

const LEVELS = ["ERROR", "WARN", "INFO", "DEBUG", "TRACE"];
const MAX_LINES = 5000;
const HISTORY_KEY = "tuff.console.history";

const LEVEL_COLOR: Record<string, string> = {
  ERROR: "text-red-600 dark:text-red-400",
  WARN: "text-amber-600 dark:text-amber-400",
  INFO: "",
  DEBUG: "text-zinc-500",
  TRACE: "text-zinc-400",
};

function time(ts: number): string {
  return new Date(ts).toLocaleTimeString([], { hour12: false });
}

function loadHistory(): string[] {
  try {
    const parsed = JSON.parse(storageGet(HISTORY_KEY) ?? "[]");
    return Array.isArray(parsed) ? parsed.filter((x) => typeof x === "string").slice(-200) : [];
  } catch {
    return [];
  }
}

export function Console({ session }: { session: Session }) {
  const [lines, setLines] = useState<Line[]>([]);
  const [paused, setPaused] = useState(false);
  const [minLevel, setMinLevel] = useState("INFO");
  const [filter, setFilter] = useState("");
  const [input, setInput] = useState("");
  const [feedback, setFeedback] = useState<string[]>([]);
  const [error, setError] = useState("");
  const [suggestions, setSuggestions] = useState<string[]>([]);
  const history = useRef<string[]>(loadHistory());
  const cursor = useRef<number>(history.current.length);
  const pending = useRef<Line[]>([]);
  const pausedRef = useRef(paused);
  const canRead = session.scopes.includes("logs.read");

  const backfill = useCallback(async () => {
    if (!canRead) return;
    try {
      const page = await api<{ lines: Line[] }>("/logs?limit=1000");
      setLines(page.lines);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, [canRead]);

  useEffect(() => {
    pausedRef.current = paused;
    if (!paused && pending.current.length) {
      const add = pending.current;
      pending.current = [];
      setLines((old) => [...old, ...add].slice(-MAX_LINES));
    }
  }, [paused]);

  useEffect(() => {
    if (!canRead) return;
    void backfill();
    return subscribe(["logs"], ["log"], (event, data) => {
      if (event === "resync") {
        void backfill();
        return;
      }
      let line: Line;
      try {
        line = JSON.parse(data);
      } catch {
        return;
      }
      if (pausedRef.current) {
        pending.current.push(line);
        if (pending.current.length > MAX_LINES) pending.current.shift();
      } else {
        setLines((old) => (old.length && old[old.length - 1].seq >= line.seq ? old : [...old, line].slice(-MAX_LINES)));
      }
    });
  }, [backfill, canRead]);

  const shown = useMemo(() => {
    const max = LEVELS.indexOf(minLevel);
    const needle = filter.toLowerCase();
    return lines.filter(
      (l) =>
        LEVELS.indexOf(l.level) <= max &&
        (!needle || l.text.toLowerCase().includes(needle) || l.target.toLowerCase().includes(needle)),
    );
  }, [lines, minLevel, filter]);
  const log = useStickToBottom<HTMLDivElement>(shown);
  const output = useRef<HTMLDivElement>(null);

  useLayoutEffect(() => {
    if (output.current) output.current.scrollTop = 0;
  }, [feedback, error]);

  const run = async () => {
    const line = input.trim();
    if (!line) return;
    history.current = [...history.current.filter((h) => h !== line), line].slice(-200);
    storageSet(HISTORY_KEY, JSON.stringify(history.current));
    cursor.current = history.current.length;
    setInput("");
    setSuggestions([]);
    setError("");
    log.stick();
    try {
      const result = await api<{ feedback: string[] }>("/console/commands", {
        method: "POST",
        body: JSON.stringify({ line }),
      });
      setFeedback([`> ${line}`, ...result.feedback]);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  };

  const complete = async () => {
    try {
      const result = await api<{ start: number; length: number; matches: string[] }>(
        `/console/complete?line=${encodeURIComponent(input)}`,
      );
      if (result.matches.length === 1) {
        setInput(input.slice(0, result.start) + result.matches[0]);
        setSuggestions([]);
      } else {
        setSuggestions(result.matches.slice(0, 50));
      }
    } catch {
      setSuggestions([]);
    }
  };

  const onKey = (e: KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "Enter") {
      e.preventDefault();
      void run();
    } else if (e.key === "Tab") {
      e.preventDefault();
      void complete();
    } else if (e.key === "ArrowUp" && history.current.length) {
      e.preventDefault();
      cursor.current = Math.max(0, cursor.current - 1);
      setInput(history.current[cursor.current] ?? "");
    } else if (e.key === "ArrowDown") {
      e.preventDefault();
      cursor.current = Math.min(history.current.length, cursor.current + 1);
      setInput(history.current[cursor.current] ?? "");
    }
  };

  const control = "rounded border border-zinc-300 bg-white px-2 py-1 text-sm dark:border-zinc-700 dark:bg-zinc-900";
  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="flex shrink-0 flex-wrap items-center gap-2 border-b border-zinc-200 px-4 py-2 dark:border-zinc-800">
        <select value={minLevel} onChange={(e) => setMinLevel(e.target.value)} className={control} aria-label="Level">
          {LEVELS.map((l) => (
            <option key={l}>{l}</option>
          ))}
        </select>
        <input
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
          placeholder="Filter"
          className={`${control} min-w-0 flex-1`}
        />
        <button type="button" onClick={() => setPaused((p) => !p)} className={control}>
          {paused ? `Resume (${pending.current.length})` : "Pause"}
        </button>
      </div>
      <div ref={log.ref} className="min-h-0 flex-1 overflow-x-hidden overflow-y-auto overscroll-contain px-4 py-2 font-mono text-xs leading-5">
        {shown.map((l) => (
          <div key={l.seq} className={`whitespace-pre-wrap break-all ${LEVEL_COLOR[l.level] ?? ""}`}>
            <span className="text-zinc-500">[{time(l.ts)}]</span>{" "}
            <span className="text-zinc-500">
              [{l.target}/{l.level}]
            </span>{" "}
            {l.text}
          </div>
        ))}
      </div>
      {(feedback.length > 0 || error) && (
        <div
          ref={output}
          className="max-h-[30%] overflow-x-hidden overflow-y-auto overscroll-contain border-t border-zinc-200 px-4 py-2 font-mono text-xs dark:border-zinc-800"
        >
          {error && <div className="text-red-600 dark:text-red-400">{error}</div>}
          {feedback.map((f, i) => (
            <div key={i} className="whitespace-pre-wrap break-all">
              {f}
            </div>
          ))}
        </div>
      )}
      {suggestions.length > 0 && (
        <div className="flex max-h-24 flex-wrap gap-1 overflow-y-auto overscroll-contain border-t border-zinc-200 px-4 py-1 font-mono text-xs dark:border-zinc-800">
          {suggestions.map((s) => (
            <span key={s} className="rounded bg-zinc-100 px-1 dark:bg-zinc-800">
              {s}
            </span>
          ))}
        </div>
      )}
      <div className="flex shrink-0 gap-2 border-t border-zinc-200 px-4 py-2 dark:border-zinc-800">
        <input
          value={input}
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={onKey}
          placeholder="Command (Tab completes, arrows browse history)"
          aria-label="Command"
          spellCheck={false}
          autoComplete="off"
          className={`${control} min-w-0 flex-1 font-mono`}
        />
        <button type="button" onClick={() => void run()} className={control}>
          Run
        </button>
      </div>
    </div>
  );
}
