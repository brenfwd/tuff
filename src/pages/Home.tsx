import { useState, type ReactNode } from "react";
import { Link } from "wouter";
import { Cubes, type CubeSpec } from "../components/Cubes";
import { VoxelIsland } from "../components/VoxelIsland";
import { PerfChart } from "../components/Perf";
import { TickRace } from "../components/TickRace";
import { ArrowRight, Check, GitHubIcon } from "../components/Icons";
import { useParallax, useReveal } from "../lib/motion";
import { useSeo } from "../lib/useSeo";
import { REPO_URL } from "../lib/site";

const INSTALL = "git clone https://github.com/brenfwd/tuff.git tuff && cd tuff && scripts/install.sh";

function CopyCommand({ command, label = "$" }: { command: string; label?: string }) {
  const [copied, setCopied] = useState(false);
  return (
    <div className="card flex items-center gap-3 overflow-hidden py-1.5 pl-4 pr-1.5 font-mono text-[0.8rem] sm:text-sm">
      <span className="select-none text-ember-text" aria-hidden="true">{label}</span>
      <code className="min-w-0 flex-1 overflow-x-auto whitespace-nowrap py-2 text-fg [scrollbar-width:none]">{command}</code>
      <button
        type="button"
        onClick={() => navigator.clipboard?.writeText(command).then(() => { setCopied(true); setTimeout(() => setCopied(false), 1500); })}
        className="shrink-0 rounded-lg px-3 py-2 font-sans text-xs font-medium text-fg-muted transition hover:bg-bg-sunken hover:text-fg"
        aria-label="Copy install command"
      >
        {copied ? "Copied" : "Copy"}
      </button>
    </div>
  );
}

function Section({ id, eyebrow, title, intro, children, cubes, tone = "base" }: { id: string; eyebrow: string; title: ReactNode; intro?: ReactNode; children: ReactNode; cubes?: CubeSpec[]; tone?: "base" | "sunken" }) {
  const ref = useReveal<HTMLElement>();
  return (
    <section id={id} ref={ref} aria-labelledby={`${id}-title`} className={`relative isolate overflow-hidden border-t border-line ${tone === "sunken" ? "bg-bg-sunken" : ""}`}>
      {cubes && <Cubes cubes={cubes} />}
      <div className="mx-auto max-w-7xl px-4 py-20 sm:px-6 sm:py-28">
        <div className="reveal max-w-3xl">
          <p className="font-mono text-xs font-semibold uppercase tracking-[0.18em] text-ember-text">{eyebrow}</p>
          <h2 id={`${id}-title`} className="mt-4 font-display text-[2rem] font-bold leading-[1.1] tracking-tight text-balance sm:text-5xl">{title}</h2>
          {intro && <p className="mt-5 text-lg leading-8 text-fg-muted text-pretty">{intro}</p>}
        </div>
        <div className="mt-12 sm:mt-16">{children}</div>
      </div>
    </section>
  );
}

function Hero() {
  const ref = useParallax<HTMLElement>();
  return (
    <section ref={ref} className="grain relative isolate overflow-hidden">
      <div className="parallax grid-bg absolute inset-0 -z-10" style={{ ["--speed" as string]: 0.15 }} aria-hidden="true" />
      <div className="parallax absolute left-1/2 top-[45%] -z-10 h-[34rem] w-[34rem] -translate-x-1/2 -translate-y-1/2 rounded-full blur-3xl md:left-[74%]" style={{ background: "radial-gradient(circle, var(--glow), transparent 65%)", ["--speed" as string]: 0.3 }} aria-hidden="true" />
      <Cubes
        cubes={[
          { side: "left", top: "62%", size: 70, rate: 160, axis: [1, 0.6, 0.2], mobile: true },
          { side: "right", top: "8%", size: 46, rate: 260, axis: [0.3, 1, 0.5] },
        ]}
      />
      <div className="mx-auto grid max-w-7xl grid-cols-[minmax(0,1fr)] items-center gap-10 px-4 pb-16 pt-14 sm:px-6 md:grid-cols-[minmax(0,1.1fr)_minmax(0,1fr)] md:pb-28 md:pt-24">
        <div className="relative z-10 min-w-0">
          <Link href="/docs/introduction" className="group inline-flex items-center gap-2 rounded-full border border-line bg-bg-raised/80 py-1 pl-1 pr-3 text-xs font-medium text-fg-muted shadow-sm backdrop-blur transition hover:border-line-strong hover:text-fg sm:text-sm">
            <span className="rounded-full bg-ember px-2 py-0.5 text-[11px] font-semibold text-white sm:text-xs">v0.1.0-alpha.1</span>
            First public alpha for Minecraft 26.3
            <ArrowRight className="h-3.5 w-3.5 transition group-hover:translate-x-0.5" />
          </Link>
          <h1 className="mt-7 font-display text-[2.75rem] font-bold leading-[1.02] tracking-[-0.035em] text-balance sm:text-6xl lg:text-7xl">
            The Minecraft server, <span className="bg-gradient-to-r from-ember-text to-ember bg-clip-text text-transparent">rebuilt in Rust.</span>
          </h1>
          <p className="mt-6 max-w-xl text-lg leading-8 text-fg-muted text-pretty sm:text-xl">
            tuff is a drop-in survival server for Java Edition 26.3. The vanilla game runs as built-in features on an inert core, and your plugins run as sandboxed WebAssembly.
          </p>
          <div className="mt-9 flex flex-col gap-3 sm:flex-row">
            <Link href="/docs/introduction" className="group inline-flex h-12 items-center justify-center gap-2 rounded-xl bg-fg px-6 font-semibold text-bg shadow-lg shadow-black/10 transition hover:-translate-y-0.5 hover:shadow-xl">
              Get started <ArrowRight className="h-4 w-4 transition group-hover:translate-x-0.5" />
            </Link>
            <a href={REPO_URL} target="_blank" rel="noopener noreferrer" className="inline-flex h-12 items-center justify-center gap-2 rounded-xl border border-line-strong bg-bg-raised/70 px-6 font-semibold backdrop-blur transition hover:-translate-y-0.5 hover:border-fg-subtle">
              <GitHubIcon className="h-5 w-5" /> View on GitHub
            </a>
          </div>
          <div className="mt-8 max-w-xl">
            <CopyCommand command={INSTALL} />
          </div>
        </div>
        <div className="relative mx-auto w-full max-w-[34rem]">
          <div className="parallax" style={{ ["--speed" as string]: -0.08 }}>
            <VoxelIsland className="float h-auto w-full drop-shadow-[0_40px_60px_rgba(0,0,0,0.35)]" />
          </div>
        </div>
      </div>
    </section>
  );
}

function Stats() {
  const ref = useReveal<HTMLDivElement>();
  const stats = [
    ["11.5x", "the vanilla tick rate", "Village benchmark, 10-day sprint"],
    ["6.5x", "less server memory", "Village benchmark, 1379 vs 8966 MiB"],
    ["20.35", "TPS with 50 players", "Stress run, vanilla held 2.15"],
    ["1.13+", "worlds load and upgrade", "Data version 1519 or newer"],
  ];
  return (
    <div ref={ref} className="relative border-y border-line bg-bg-raised/60">
      <dl className="mx-auto grid max-w-7xl grid-cols-2 px-4 sm:px-6 lg:grid-cols-4">
        {stats.map(([v, l, s], i) => (
          <div key={l} className={`reveal border-line px-2 py-8 sm:px-6 ${i % 2 ? "border-l" : ""} ${i === 2 ? "border-t lg:border-l lg:border-t-0" : ""} ${i === 3 ? "border-t lg:border-t-0" : ""}`} style={{ ["--delay" as string]: `${i * 90}ms` }}>
            <dt className="sr-only">{l}</dt>
            <dd>
              <span className="block font-display text-3xl font-bold tracking-tight sm:text-5xl">{v}</span>
              <span className="mt-1 block text-sm font-medium sm:text-base">{l}</span>
              <span className="mt-1 block text-xs text-fg-subtle">{s}</span>
            </dd>
          </div>
        ))}
      </dl>
    </div>
  );
}

const builtins = ["worldgen", "redstone", "items", "maps", "recipes", "loot", "blocks", "combat", "players", "entities", "mobs", "survival", "dimensions", "core-commands", "vanilla-commands", "anticheat"];

function Architecture() {
  return (
    <Section
      id="architecture"
      eyebrow="Architecture"
      title={<>Vanilla is a plugin. <span className="text-fg-muted">So is everything else.</span></>}
      intro="The core does nothing on its own. The vanilla game is 16 built-in features loaded on top of it, and external plugins are WebAssembly modules the server loads, unloads and reloads while it runs."
      cubes={[
        { side: "right", top: "18%", size: 120, rate: 200, axis: [0.5, 1, 0.1] },
        { side: "left", top: "70%", size: 64, rate: 300, axis: [1, 0.3, 0.6], mobile: true },
      ]}
    >
      <div className="grid grid-cols-[minmax(0,1fr)] gap-5 lg:grid-cols-[minmax(0,1fr)_minmax(0,1fr)] lg:gap-8">
        <div className="space-y-3">
          {[
            { tag: "Layer 3", title: "Runtime plugins", body: "Sandboxed .wasm modules on the pure-Rust wasmi interpreter. Hot load, unload and reload. Nothing is allowed until the owner grants it.", accent: true },
            { tag: "Layer 2", title: "16 built-in features", body: "The vanilla game, compiled in and on by default. Leave one out with tuff.vanilla.<name>.enabled=false, for example on a minigame server.", chips: builtins },
            { tag: "Layer 1", title: "Inert core", body: "Networking, worlds, ticking and storage. Features are layered on top, and the loaded set is logged at start-up." },
          ].map((l, i) => (
            <div key={l.title} className={`reveal card card-hover p-5 sm:p-6 ${l.accent ? "border-ember/40" : ""}`} style={{ ["--delay" as string]: `${i * 110}ms`, marginLeft: `${(2 - i) * 0}rem` }}>
              <div className="flex items-center gap-3">
                <span className={`grid h-9 w-9 shrink-0 place-items-center rounded-lg font-mono text-xs font-bold ${l.accent ? "bg-ember text-white" : "bg-bg-sunken text-fg-muted ring-1 ring-line"}`}>{3 - i}</span>
                <div>
                  <p className="font-mono text-[11px] uppercase tracking-[0.14em] text-fg-subtle">{l.tag}</p>
                  <h3 className="font-display text-lg font-semibold">{l.title}</h3>
                </div>
              </div>
              <p className="mt-3 text-[0.95rem] leading-7 text-fg-muted">{l.body}</p>
              {l.chips && (
                <ul className="mt-4 flex flex-wrap gap-1.5" aria-label="Built-in features">
                  {l.chips.map((c) => <li key={c} className="rounded-md border border-line bg-bg-sunken px-2 py-0.5 font-mono text-xs text-fg-muted">{c}</li>)}
                </ul>
              )}
            </div>
          ))}
        </div>
        <div className="reveal card flex flex-col p-6 sm:p-8" style={{ ["--delay" as string]: "200ms" }}>
          <h3 className="font-display text-xl font-semibold">Optional features, off by default</h3>
          <p className="mt-2 text-[0.95rem] leading-7 text-fg-muted">Switch one on with <code className="rounded bg-bg-sunken px-1.5 py-0.5 font-mono text-[0.85em]">--config tuff.features.&lt;id&gt;.enabled true</code>.</p>
          <ul className="mt-6 divide-y divide-line">
            {[
              ["audit-log", "A world audit log in its own SQLite file, with lookup, rollback, restore and purge."],
              ["world-map", "A live, zoomable world map page in the web panel, with chunk pre-generation."],
              ["moving-block-entities", "Pistons move chests, furnaces and other block entities with their data."],
              ["chunk-progress", "A boss bar showing each player how many chunks in view have been sent."],
            ].map(([id, d]) => (
              <li key={id} className="flex gap-4 py-4">
                <Check className="mt-1 h-4 w-4 shrink-0 text-ember-text" />
                <div>
                  <p className="font-mono text-sm font-semibold">{id}</p>
                  <p className="mt-1 text-sm leading-6 text-fg-muted">{d}</p>
                </div>
              </li>
            ))}
          </ul>
          <Link href="/docs/features" className="mt-auto inline-flex items-center gap-1.5 pt-4 text-sm font-semibold text-ember-text hover:underline">All built-in features <ArrowRight className="h-4 w-4" /></Link>
        </div>
      </div>
    </Section>
  );
}

const features: { title: string; body: string; href: string; icon: ReactNode }[] = [
  { title: "Sandboxed WebAssembly plugins", body: "A plugin that traps, runs out of fuel or misuses the ABI is unloaded on the spot, and the server carries on.", href: "/docs/plugins", icon: <path d="M4 7l8-4 8 4v10l-8 4-8-4z M4 7l8 4 8-4 M12 11v10" /> },
  { title: "Drop-in for your world", body: "Worlds from Minecraft 1.13 (data version 1519) or newer load and are upgraded. Keep a folder world, or the whole server in one SQLite file.", href: "/docs/running", icon: <path d="M3 12a9 9 0 1 0 18 0 9 9 0 0 0-18 0 M3 12h18 M12 3c3 3.5 3 14.5 0 18 M12 3c-3 3.5-3 14.5 0 18" /> },
  { title: "Web admin panel", body: "Access, World, Config, Audit, Players, Logs, Metrics, Timings, Backups, Datapacks, Resource packs and Anticheat pages, each behind its own permission.", href: "/docs/admin-panel", icon: <path d="M3 5h18v12H3z M8 21h8 M12 17v4 M7 13l3-3 2 2 4-4" /> },
  { title: "Anti-cheat", body: "Checks movement, reach, rates, item and inventory integrity, and vehicles. It only logs by default; turn on enforcement when you are ready.", href: "/docs/anti-cheat", icon: <path d="M12 3l8 3v6c0 4.5-3.4 8.3-8 9-4.6-.7-8-4.5-8-9V6z M9 12l2 2 4-4" /> },
  { title: "Anti-xray", body: "Valuable blocks nobody can see are hidden, and sent for real once they are exposed or mined towards.", href: "/docs/anti-cheat", icon: <path d="M2 12s3.5-7 10-7 10 7 10 7-3.5 7-10 7S2 12 2 12z M12 9a3 3 0 1 0 0 6 3 3 0 0 0 0-6z M4 20L20 4" /> },
  { title: "Chunk pre-generation", body: "Generate a circle or square of terrain ahead of players. Chunks a player is loading take priority, and the work runs in parallel.", href: "/docs/pre-generating", icon: <path d="M4 4h6v6H4z M14 4h6v6h-6z M4 14h6v6H4z M14 14h6v6h-6z" /> },
  { title: "Crowd mode", body: "An opt-in for mob farms: a region holding 1500 or more entities runs its mobs simplified, while tempting, breeding and targeting still work.", href: "/docs/settings", icon: <path d="M8 11a3 3 0 1 0 0-6 3 3 0 0 0 0 6z M16 11a3 3 0 1 0 0-6 3 3 0 0 0 0 6z M2 20c0-3 2.7-5 6-5s6 2 6 5 M14 15c3.3 0 8 1 8 5" /> },
  { title: "Data packs", body: "A world loads its own data packs. Functions, tags, recipes, loot tables, predicates and advancements are applied.", href: "/docs/data-packs", icon: <path d="M4 7l8-4 8 4-8 4z M4 12l8 4 8-4 M4 17l8 4 8-4" /> },
  { title: "Custom content pack", body: "The server builds one resource pack from the items, blocks and assets plugins register, and offers it to every client.", href: "/docs/features", icon: <path d="M4 8l8-5 8 5v8l-8 5-8-5z M12 13l8-5 M12 13 4 8 M12 13v8" /> },
];

function Features() {
  return (
    <Section
      id="features"
      eyebrow="What's inside"
      title="Everything a survival server needs, out of the box."
      intro="The tools for running a real community are part of the server, documented and switched on with a setting."
      tone="sunken"
      cubes={[
        { side: "left", top: "10%", size: 96, rate: 240, axis: [0.8, 0.5, 0.3], mobile: true },
        { side: "right", top: "64%", size: 140, rate: 150, axis: [0.2, 1, 0.4] },
        { side: "right", top: "6%", size: 40, rate: 380, axis: [1, 1, 0], offset: 4 },
      ]}
    >
      <ul className="grid grid-cols-[minmax(0,1fr)] gap-4 sm:grid-cols-2 lg:grid-cols-3">
        {features.map((f, i) => (
          <li key={f.title} className="reveal" style={{ ["--delay" as string]: `${(i % 3) * 90}ms` }}>
            <Link href={f.href} className="card card-hover group flex h-full flex-col p-6">
              <span className="grid h-11 w-11 place-items-center rounded-xl bg-ember-soft text-ember-text ring-1 ring-ember/20">
                <svg viewBox="0 0 24 24" className="h-5 w-5" fill="none" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">{f.icon}</svg>
              </span>
              <h3 className="mt-5 font-display text-lg font-semibold">{f.title}</h3>
              <p className="mt-2 flex-1 text-[0.95rem] leading-7 text-fg-muted">{f.body}</p>
              <span className="mt-4 inline-flex items-center gap-1 text-sm font-medium text-fg-subtle transition group-hover:text-ember-text">Read more <ArrowRight className="h-3.5 w-3.5" /></span>
            </Link>
          </li>
        ))}
      </ul>
    </Section>
  );
}

function Performance() {
  return (
    <Section
      id="performance"
      eyebrow="Performance"
      title={<>More ticks. <span className="text-fg-muted">Less memory.</span></>}
      intro="Measured side by side on a 16-vCPU Linux machine: tuff built with the release-opt profile against vanilla 26.3 on its bundled OpenJDK 25 with an 8 GB heap and Aikar's G1 flags. Both servers were warmed up first. Three alternating runs per server; medians shown."
      cubes={[
        { side: "left", top: "30%", size: 110, rate: 210, axis: [0.6, 1, 0.2] },
        { side: "right", top: "82%", size: 58, rate: 320, axis: [1, 0.4, 0.5], mobile: true },
      ]}
    >
      <div className="grid grid-cols-[minmax(0,1fr)] gap-6 lg:grid-cols-[minmax(0,1fr)_22rem]">
        <PerfChart />
        <div className="reveal space-y-4" style={{ ["--delay" as string]: "150ms" }}>
          <TickRace />
        </div>
      </div>
      <div className="reveal mt-8 grid grid-cols-[minmax(0,1fr)] gap-4 text-sm leading-6 text-fg-muted md:grid-cols-2">
        <p className="card p-5"><strong className="text-fg">tuff parallelises ticking across cores, so it uses more CPU:</strong> 2.70 cores against 1.02 in the village run, and 15.35 against 3.90 in the stress run, where it is near saturation on 16 cores. That makes the mean-tick ratio a fairer capacity comparison than TPS.</p>
        <p className="card p-5"><strong className="text-fg">Vanilla's memory is high partly because its 8 GB heap is pre-touched.</strong> Reproduce the runs with <code className="font-mono text-[0.85em] text-fg">cargo bench --bench village -- --server both</code> or the <code className="font-mono text-[0.85em] text-fg">stress</code> bench.</p>
      </div>
    </Section>
  );
}

const pluginCode = `use tuff_plugin_sdk::prelude::*;

static VISITS: StoredMap<u32> = StoredMap::new("visits");

struct Hello;

impl Plugin for Hello {
    fn register() -> Result<(), String> {
        command("hello")
            .about("Greets you")
            .executes(|ctx| {
                let player = ctx.player()?;
                let visits = VISITS.update(player.uuid, |n| n.unwrap_or(0) + 1)?;
                ctx.reply(text!("Hello, ", player.name.aqua(), "! ",
                    format!("(visit {visits})").gray()))?;
                Ok(visits as i32)
            })
            .help()
            .register()?;
        Ok(())
    }
}

plugin!(Hello {
    id: "hello",
    version: "0.1.0",
    description: "Greets players and counts their visits",
});`;

function tokenize(line: string) {
  const re = /(\/\/.*$)|("(?:[^"\\]|\\.)*")|\b(use|static|struct|impl|fn|let|for|Ok|Result|String|u32|i32|as)\b|\b([A-Z][A-Za-z0-9_]*)\b|\b([a-z_][a-z0-9_]*)(?=!?\()|(\w+!)/g;
  const out: ReactNode[] = [];
  let last = 0;
  let m: RegExpExecArray | null;
  let k = 0;
  while ((m = re.exec(line))) {
    if (m.index > last) out.push(line.slice(last, m.index));
    const cls = m[1] ? "text-fg-subtle italic" : m[2] ? "text-[#a5d6a7] dark:text-[#9ccf8f]" : m[3] ? "text-[#ff9b6b]" : m[4] ? "text-[#7fb6ff]" : m[6] ? "text-[#e6b450]" : "text-[#e8dccb]";
    out.push(<span key={k++} className={cls}>{m[0]}</span>);
    last = m.index + m[0].length;
  }
  out.push(line.slice(last));
  return out;
}

function Plugins() {
  return (
    <Section
      id="plugins"
      eyebrow="Plugins"
      title="Write plugins in Rust or C++. Ship a single .wasm file."
      intro="The SDK wraps the plugin ABI with a command builder, typed events, timers, async tasks, settings and per-plugin storage. Implement the Plugin trait, declare it with plugin!, and the manifest is written into the module."
      tone="sunken"
      cubes={[{ side: "left", top: "15%", size: 84, rate: 260, axis: [0.4, 1, 0.6], mobile: true }, { side: "right", top: "75%", size: 100, rate: 190, axis: [1, 0.5, 0.2] }]}
    >
      <div className="grid grid-cols-[minmax(0,1fr)] gap-6 lg:grid-cols-[minmax(0,1.25fr)_minmax(0,1fr)] lg:gap-10">
        <div className="reveal overflow-hidden rounded-2xl border border-[#2b2724] bg-[#12100f] shadow-2xl shadow-black/30">
          <div className="flex items-center gap-2 border-b border-[#2b2724] px-4 py-3">
            <span className="h-3 w-3 rounded-full bg-[#3a3532]" /><span className="h-3 w-3 rounded-full bg-[#3a3532]" /><span className="h-3 w-3 rounded-full bg-[#3a3532]" />
            <span className="ml-3 font-mono text-xs text-[#a39c94]">examples/rust-hello/src/lib.rs</span>
          </div>
          <pre className="overflow-x-auto p-5 font-mono text-[0.78rem] leading-6 text-[#e8dccb] sm:text-[0.84rem]"><code>{pluginCode.split("\n").map((l, i) => <span key={i} className="block">{tokenize(l)}{l === "" ? "\u00a0" : null}</span>)}</code></pre>
        </div>
        <div className="reveal space-y-5" style={{ ["--delay" as string]: "120ms" }}>
          {[
            ["Capability grants", "A plugin requests files, http, websocket, tcp, udp, routes, listeners or dimensions in its manifest. Nothing is allowed until the owner grants it, in game or offline."],
            ["Fault isolation", "A plugin that traps or runs out of fuel is unloaded with the plugins that depend on it, and the server keeps running."],
            ["Hot reload", "Copy the .wasm into plugins/ and run /tuff plugin load hello. Unload and reload without a restart."],
            ["Your license, your call", "The SDK, ABI and examples are MIT. Plugins written against them are your own work, and may be closed source."],
          ].map(([t, b]) => (
            <div key={t} className="flex gap-4">
              <span className="mt-1 grid h-6 w-6 shrink-0 place-items-center rounded-md bg-ember text-white"><Check className="h-3.5 w-3.5" /></span>
              <div>
                <h3 className="font-semibold">{t}</h3>
                <p className="mt-1 text-[0.95rem] leading-7 text-fg-muted">{b}</p>
              </div>
            </div>
          ))}
          <p className="text-sm text-fg-subtle">Examples include a WorldEdit plugin, a custom dimension, plugin-to-plugin messaging, C++ plugins and a hand-written WebAssembly text plugin.</p>
          <Link href="/docs/plugins" className="inline-flex items-center gap-1.5 font-semibold text-ember-text hover:underline">Plugin guide <ArrowRight className="h-4 w-4" /></Link>
        </div>
      </div>
    </Section>
  );
}

function GetStarted() {
  const steps = [
    { n: "01", title: "Build it on your machine", body: "tuff publishes no prebuilt binaries: the build downloads Mojang's server jar and embeds data generated from it. The script asks you to accept the Minecraft EULA first.", cmd: INSTALL },
    { n: "02", title: "Start the server", body: "The binary lands in ./tuff-server/tuff. It listens on port 25565 on every IPv4 and IPv6 interface. Players sign in with their Microsoft accounts.", cmd: "cd tuff-server && ./tuff" },
    { n: "03", title: "Let your players in", body: "A new server has white-list=true. Add players on the console, then join localhost:25565 with a 26.3 client.", cmd: "whitelist add <name>" },
  ];
  return (
    <Section
      id="get-started"
      eyebrow="Get started"
      title="From clone to first join in three steps."
      intro="Linux and macOS are supported. Windows is best effort: build and run under WSL or Docker. A release build took about 6.5 minutes on a 16-vCPU machine."
      cubes={[{ side: "right", top: "20%", size: 76, rate: 280, axis: [0.7, 1, 0.3], mobile: true }]}
    >
      <ol className="grid grid-cols-[minmax(0,1fr)] gap-4 lg:grid-cols-3">
        {steps.map((s, i) => (
          <li key={s.n} className="reveal card flex min-w-0 flex-col p-6" style={{ ["--delay" as string]: `${i * 110}ms` }}>
            <span className="font-mono text-sm font-semibold text-ember-text">{s.n}</span>
            <h3 className="mt-3 font-display text-xl font-semibold">{s.title}</h3>
            <p className="mt-2 flex-1 text-[0.95rem] leading-7 text-fg-muted">{s.body}</p>
            <code className="mt-5 block overflow-x-auto whitespace-nowrap rounded-lg border border-line bg-bg-sunken px-3 py-2.5 font-mono text-xs text-fg">{s.cmd}</code>
          </li>
        ))}
      </ol>
      <div className="reveal relative mt-16 overflow-hidden rounded-3xl border border-line bg-[#12100f] px-6 py-14 text-center text-[#f5f4f2] sm:px-12">
        <div className="absolute inset-0 -z-0 opacity-70" style={{ background: "radial-gradient(60% 80% at 50% 0%, rgba(224,96,26,0.35), transparent 70%)" }} aria-hidden="true" />
        <div className="relative">
          <h2 className="font-display text-3xl font-bold tracking-tight text-balance sm:text-4xl">Ready to run something fast?</h2>
          <p className="mx-auto mt-4 max-w-xl text-[#c9c3bc]">Read the docs, build the server, and bring your world with you.</p>
          <div className="mt-8 flex flex-col justify-center gap-3 sm:flex-row">
            <Link href="/docs/installing" className="inline-flex h-12 items-center justify-center gap-2 rounded-xl bg-ember px-6 font-semibold text-white transition hover:-translate-y-0.5 hover:bg-[#ea6a24]">Install tuff <ArrowRight className="h-4 w-4" /></Link>
            <a href={REPO_URL} target="_blank" rel="noopener noreferrer" className="inline-flex h-12 items-center justify-center gap-2 rounded-xl border border-white/20 px-6 font-semibold transition hover:-translate-y-0.5 hover:border-white/40"><GitHubIcon className="h-5 w-5" /> Star on GitHub</a>
          </div>
        </div>
      </div>
    </Section>
  );
}

export default function Home() {
  useSeo({ title: "tuff - a fast Minecraft server in Rust with WebAssembly plugins" });
  return (
    <main id="main" tabIndex={-1} className="outline-none">
      <Hero />
      <Stats />
      <Architecture />
      <Features />
      <Performance />
      <Plugins />
      <GetStarted />
    </main>
  );
}
