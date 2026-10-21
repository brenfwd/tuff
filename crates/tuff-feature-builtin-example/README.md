# tuff-feature-builtin-example

A worked example of every surface core offers features (see the crate docs in
`src/lib.rs`). This file covers the one with a build step of its own: the
web admin panel page. The shipped server loads it only when asked to, with
the `tuff-feature-builtin-example` feature (`tuff.features.example.enabled`).

This feature is compiled into the server. Plugins loaded at run time are
WebAssembly modules instead; the top-level README's "Runtime plugins
(WebAssembly)" section covers them, and `examples/rust-hello` is the
Rust example.

## Panel page and API

The feature adds a "Gold" page to the web panel (`/admin/`, enabled with
`tuff.web.enabled`) that shows the world's gold count live and resets it,
backed by an API at `/admin/api/v1/plugins/example-gold`, and it adds a
"Gold placed" card to the dashboard.

- `panel/` is a React + TypeScript + Tailwind app built with Vite and bun.
  `build.rs` runs `bun install --frozen-lockfile` and `bun run build` there
  and writes `gold.js` and `gold.css` to `$OUT_DIR/panel/`; `src/lib.rs`
  embeds both with `include_str!` and registers them as a `PanelPage`.
- `panel/src/main.tsx` exports `mount(element, ctx)`: it renders the page
  into `element` with `createRoot` and returns a cleanup that unmounts it
  (deferred with `queueMicrotask`, since the shell calls it while
  rendering).
- `panel/src/GoldPage.tsx` uses the `ctx` API the way every page should:
  `ctx.api` for JSON calls (CSRF handled), `ctx.events` for live updates
  (refetch on `resync`), `ctx.toast` for feedback.

### Writing a page for another feature

1. Copy `panel/` (package.json, bun.lock, tsconfig.json, vite.config.ts,
   src/) and `build.rs` into the feature crate; set `build = "build.rs"` in
   its Cargo.toml. Change the `name` given to `panelPage` (the output file
   names) and the page id/topics in the source.
2. Keep `panelPage(...)` from `crates/tuff-web-ui/app/sdk/vite.ts` in the Vite
   config. It builds an ES module library and resolves `react`,
   `react-dom`, `react-dom/client` and `react/jsx-runtime` to shims over
   the shell's React (exposed on `globalThis.__tuffPanel`), so the page
   does not ship a second React and shares the shell's version. Depend on
   `@types/react` only, never on `react` itself.
3. Import types from `crates/tuff-web-ui/app/src/sdk/context.ts` (`Mount`,
   `PanelContext`).
   Shared components (`Table` with paging, `LineChart`, `Field`, `Stat`,
   `Button`) come from `@tuff/panel-ui`, which `panelPage` also resolves
   to the shell's copy; add the same `paths` entry as `panel/tsconfig.json`
   for type checking. The Gold page uses `Stat` and `Button`.
4. Style with Tailwind utilities; `src/index.css` imports only Tailwind's
   theme and utilities layers (no preflight, which the shell already has)
   and the shell's `dark` variant, so pages look native in both themes.
5. In `Feature::load` (`Phase::Content`), register the page and its API:
   `registries.panel.register_page(ID, PanelPage::new(id, title, scope,
   module).style(css))` and `registries.panel.register_api(ID,
   PanelApi::new(id, scope, api_handler(handler)))`. Ids are a-z, 0-9, `-`
   and `_`; a duplicate id fails startup naming both features.
6. Scopes are `Scope::Plugin("<plugin>.<what>")`. The page's scope gates
   listing it and fetching its assets; the API's scope gates every call,
   and handlers check finer scopes with `auth.require(scope)`. Today every
   feature scope needs operator level 4; permission nodes will refine that.
7. Record actions with `shared.web.audit.record(...)` and publish live
   updates with `shared.web.events.publish("plugin.<page id>.<name>",
   event, json)`; pages subscribe with `ctx.events`. Publish from every
   place the value changes, not only from the API's own handlers: `GoldCounter`
   (`src/lib.rs`) keeps a `OnceLock<PanelEvents>` set in `WorldService::start`
   and calls `publish` from both `add()` (the region hook, on every gold
   block placed) and `reset()` (the API handler), so the page updates with
   no refresh either way. `PanelEvents::publish` is a non-blocking
   `try_send`, so it is safe to call from a synchronous region hook; a
   placement burst is throttled to one publish per
   `GOLD_PUBLISH_INTERVAL` (a `Mutex<Option<Instant>>` last-published
   timestamp) so it never floods the bus, while `reset()` always publishes
   immediately regardless of the throttle.
8. For a dashboard metric card, feed a series once with
   `shared.metrics.series(name, Kind::Gauge | Kind::Counter)` (dotted
   lowercase name: a-z, 0-9, `.`, `_`, `-`) and keep the handle. Register
   the card with `registries.panel.register_card(ID, MetricCard::new(series,
   title).unit(Unit::Bytes).scope(scope))`; unit defaults to `Count` (also
   `Bytes`, `Millis`), and scope defaults to `metrics.read` plus the given
   scope, so only sessions holding both see the card. A counter's card
   shows its rate per second. A duplicate series fails startup naming both
   features.

Page scripts load under the panel's CSP: the shell's policy allows scripts
and stylesheets only from `/admin/pages/` on the panel's own origin, with
no `unsafe-inline` or `eval`, so pages must not inject inline scripts or
`style` attributes in markup (React's `style` prop is fine).
