import { useCallback, useEffect, useState } from "react";
import { Button, Stat } from "@tuff/panel-ui";
import type { PanelContext } from "../../../web-ui/app/src/sdk/context";

const API = "/plugins/example-gold";
const TOPIC = "plugin.example-gold.count";

interface Count {
  goldPlaced: number;
}

function message(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

export function GoldPage({ ctx }: { ctx: PanelContext }) {
  const [count, setCount] = useState<number | null>(null);
  const [busy, setBusy] = useState(false);

  const refresh = useCallback(() => {
    ctx
      .api<Count>(API)
      .then((c) => setCount(c.goldPlaced))
      .catch((e) => ctx.toast(message(e), "error"));
  }, [ctx]);

  useEffect(refresh, [refresh]);

  useEffect(
    () =>
      ctx.events([TOPIC], ["count"], (event, data) => {
        if (event === "count") setCount((JSON.parse(data) as Count).goldPlaced);
        else refresh();
      }),
    [ctx, refresh],
  );

  const reset = async () => {
    setBusy(true);
    try {
      const c = await ctx.api<Count>(`${API}/reset`, { method: "POST", body: "{}" });
      setCount(c.goldPlaced);
      ctx.toast("Gold counter reset");
    } catch (e) {
      ctx.toast(message(e), "error");
    } finally {
      setBusy(false);
    }
  };

  return (
    <section className="flex flex-col gap-4 p-4">
      <h1 className="text-lg font-semibold">Gold</h1>
      <Stat label="Gold blocks placed in this world" value={count ?? "-"} />
      <div>
        <Button onClick={reset} disabled={busy}>
          Reset counter
        </Button>
      </div>
    </section>
  );
}
