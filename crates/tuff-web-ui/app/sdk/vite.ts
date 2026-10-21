// Vite settings for a plugin page of the tuff web panel. A plugin's
// vite.config.ts spreads `panelPage(...)` into its config (next to the
// React and Tailwind plugins) to get:
// - an ES module library build of `entry` named `<name>.js`, plus
//   `<name>.css` when the page imports CSS, written to TUFF_PANEL_OUT
//   (the crate's build.rs sets it) or `dist`;
// - `react`, `react-dom`, `react-dom/client` and `react/jsx-runtime`
//   resolved to shims over the shell's own React, so no second copy ships;
// - `@tuff/panel-ui` resolved to the shell's component kit the same way.
import { fileURLToPath } from "node:url";

const shim = (file: string) => fileURLToPath(new URL(`../src/sdk/shims/${file}`, import.meta.url));

export interface PanelPageOptions {
  /** The module exporting `mount(element, ctx)`. */
  entry: string;
  /** Output base name: `<name>.js` and `<name>.css`. */
  name: string;
}

export function panelPage({ entry, name }: PanelPageOptions) {
  return {
    resolve: {
      alias: [
        { find: /^react\/jsx-runtime$/, replacement: shim("jsx-runtime.ts") },
        { find: /^react\/jsx-dev-runtime$/, replacement: shim("jsx-runtime.ts") },
        { find: /^react-dom\/client$/, replacement: shim("react-dom-client.ts") },
        { find: /^react-dom$/, replacement: shim("react-dom.ts") },
        { find: /^react$/, replacement: shim("react.ts") },
        { find: /^@tuff\/panel-ui$/, replacement: shim("ui.ts") },
      ],
    },
    define: { "process.env.NODE_ENV": JSON.stringify("production") },
    build: {
      outDir: process.env.TUFF_PANEL_OUT ?? "dist",
      emptyOutDir: true,
      reportCompressedSize: false,
      modulePreload: false,
      lib: {
        entry,
        formats: ["es" as const],
        fileName: () => `${name}.js`,
        cssFileName: name,
      },
    },
  };
}
