import { useEffect, useRef, useState } from "react";
import { Compartment, EditorState } from "@codemirror/state";
import { EditorView, keymap } from "@codemirror/view";
import { defaultKeymap, history, historyKeymap } from "@codemirror/commands";
import { defaultHighlightStyle, syntaxHighlighting } from "@codemirror/language";
import { api } from "./api";
import { flatten, miniMessage, runCss, type Run } from "./minimessage";

/** How the server reads the field: MiniMessage, `&` codes, both, or the
 * section-sign codes vanilla text such as the motd uses. */
export type TextFormat = "minimessage" | "legacy" | "mixed" | "section";

const miniMessageLanguage = miniMessage();

const baseTheme = EditorView.theme({
  "&": { fontSize: "13px", border: "1px solid #a1a1aa66", borderRadius: "4px", padding: "2px 4px" },
  ".cm-scroller": { fontFamily: "ui-monospace, SFMono-Regular, Menlo, Consolas, monospace" },
  "&.cm-focused": { outline: "none", borderColor: "#7c3aed" },
});
const lightTheme = EditorView.theme({ "&": { backgroundColor: "#ffffff", color: "#18181b" } }, { dark: false });
const darkTheme = EditorView.theme({ "&": { backgroundColor: "#18181b", color: "#e4e4e7" } }, { dark: true });

function useDarkClass(): boolean {
  const [dark, setDark] = useState(() => document.documentElement.classList.contains("dark"));
  useEffect(() => {
    const el = document.documentElement;
    const obs = new MutationObserver(() => setDark(el.classList.contains("dark")));
    obs.observe(el, { attributes: true, attributeFilter: ["class"] });
    return () => obs.disconnect();
  }, []);
  return dark;
}

/** Styled runs of a component, as chat would show them. */
export function TextPreview({ runs }: { runs: Run[] }) {
  return (
    <span className="whitespace-pre-wrap">
      {runs.map((r, i) => (
        <span key={i} style={runCss(r.style)} title={r.style.hover ?? r.style.click}>
          {r.text}
        </span>
      ))}
    </span>
  );
}

/** The runs the server parses `text` into, refreshed as it changes. */
export function usePreview(text: string, format: TextFormat = "minimessage"): Run[] | null {
  const [runs, setRuns] = useState<Run[] | null>(null);
  useEffect(() => {
    let live = true;
    const timer = window.setTimeout(async () => {
      try {
        const r = await api<{ component: Parameters<typeof flatten>[0] }>("/text/preview", {
          method: "POST",
          body: JSON.stringify({ text, format }),
        });
        if (live) setRuns(flatten(r.component));
      } catch {
        if (live) setRuns(null);
      }
    }, 150);
    return () => {
      live = false;
      window.clearTimeout(timer);
    };
  }, [text, format]);
  return runs;
}

/** A MiniMessage editor with highlighting and a live preview on a chat
 * coloured background. `onCommit` runs on blur or Enter. */
export function MiniMessageField({
  value,
  onCommit,
  format = "minimessage",
  placeholder,
  label,
}: {
  value: string;
  onCommit: (text: string) => void;
  format?: TextFormat;
  placeholder?: string;
  label?: string;
}) {
  const host = useRef<HTMLDivElement>(null);
  const view = useRef<EditorView | null>(null);
  const themeC = useRef(new Compartment());
  const [text, setText] = useState(value);
  const commit = useRef(onCommit);
  commit.current = onCommit;
  const dark = useDarkClass();
  const runs = usePreview(text, format);

  useEffect(() => {
    if (!host.current) return;
    const shadow = host.current.shadowRoot ?? host.current.attachShadow({ mode: "open" });
    const v = new EditorView({
      root: shadow,
      parent: shadow,
      state: EditorState.create({
        doc: value,
        extensions: [
          history(),
          keymap.of([
            {
              key: "Enter",
              run: (ed) => {
                commit.current(ed.state.doc.toString());
                return true;
              },
            },
            ...defaultKeymap,
            ...historyKeymap,
          ]),
          EditorState.transactionFilter.of((tr) => (tr.newDoc.lines > 1 ? [] : tr)),
          miniMessageLanguage,
          syntaxHighlighting(defaultHighlightStyle, { fallback: true }),
          baseTheme,
          themeC.current.of(lightTheme),
          EditorView.lineWrapping,
          EditorView.updateListener.of((u) => {
            if (u.docChanged) setText(u.state.doc.toString());
            if (u.focusChanged && !u.view.hasFocus) commit.current(u.state.doc.toString());
          }),
          EditorView.contentAttributes.of({ "aria-label": label ?? "text", "data-placeholder": placeholder ?? "" }),
        ],
      }),
    });
    view.current = v;
    return () => {
      v.destroy();
      view.current = null;
    };
  }, []);

  useEffect(() => {
    const v = view.current;
    if (v && v.state.doc.toString() !== value && !v.hasFocus) {
      v.dispatch({ changes: { from: 0, to: v.state.doc.length, insert: value } });
    }
  }, [value]);

  useEffect(() => {
    view.current?.dispatch({ effects: themeC.current.reconfigure(dark ? darkTheme : lightTheme) });
  }, [dark]);

  return (
    <div className="min-w-64 flex-1">
      <div ref={host} />
      <div className="mt-1 min-h-6 rounded bg-zinc-800/90 px-2 py-0.5 font-mono text-sm text-white">
        {runs ? <TextPreview runs={runs} /> : <span className="text-zinc-400">{text ? "..." : placeholder}</span>}
      </div>
    </div>
  );
}
