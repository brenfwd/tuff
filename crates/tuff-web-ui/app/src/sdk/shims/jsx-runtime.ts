// Plugin builds resolve `react/jsx-runtime` here, the target of compiled JSX.
import type { Shared } from "../shared";

const runtime = (globalThis as unknown as { __tuffPanel: Shared }).__tuffPanel.JsxRuntime;

export const { Fragment, jsx, jsxs } = runtime;
