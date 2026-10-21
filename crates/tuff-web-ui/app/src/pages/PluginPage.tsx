import { useEffect, useRef, useState } from "react";
import { api, Session } from "../api";
import { HostBridge, type HostMessage } from "../frameBridge";
import { openEventSource, PageEvents, Toast } from "../plugins";
import type { PanelPageInfo } from "../sdk/context";

const FRAME_URL = "/admin/frame.html";

async function text(url: string): Promise<string> {
  const resp = await fetch(url, { credentials: "same-origin" });
  if (!resp.ok) throw new Error(`${url}: ${resp.status}`);
  return resp.text();
}

export function PluginPage({ page, session, toast }: { page: PanelPageInfo; session: Session; toast: Toast }) {
  const frame = useRef<HTMLIFrameElement>(null);
  const [error, setError] = useState("");
  const [height, setHeight] = useState(240);

  useEffect(() => {
    const element = frame.current;
    if (!element) return;
    setError("");
    const events = new PageEvents(openEventSource);
    const post = (message: HostMessage) => element.contentWindow?.postMessage(message, "*");
    const bridge = new HostBridge({
      page,
      post,
      api: (path, init) => api(path, init),
      subscribe: (topics, names, handler) => events.subscribe(topics, names, handler),
      toast,
      setHeight,
    });
    let cancelled = false;
    const onMessage = (e: MessageEvent) => {
      if (e.source !== element.contentWindow) return;
      if (e.data?.type === "ready") {
        Promise.all([text(page.module), Promise.all(page.styles.map(text))])
          .then(([module, styles]) => {
            if (cancelled) return;
            post({
              type: "init",
              page,
              principal: session.principal,
              scopes: [...session.scopes],
              module,
              styles,
              dark: document.documentElement.classList.contains("dark"),
            });
          })
          .catch((err) => {
            if (!cancelled) setError(`${page.title} failed to load: ${err instanceof Error ? err.message : String(err)}`);
          });
        return;
      }
      bridge.handle(e.data);
    };
    window.addEventListener("message", onMessage);
    element.src = FRAME_URL;
    return () => {
      cancelled = true;
      window.removeEventListener("message", onMessage);
      bridge.close();
      events.close();
    };
  }, [page, session, toast]);

  return (
    <div className="flex flex-col">
      {error && <p className="p-4 text-red-600">{error}</p>}
      <iframe
        ref={frame}
        title={page.title}
        sandbox="allow-scripts"
        referrerPolicy="no-referrer"
        className="w-full border-0"
        style={{ height }}
      />
    </div>
  );
}
