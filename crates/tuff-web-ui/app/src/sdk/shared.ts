// The shell's React, shared with plugin page modules so they do not ship a
// second copy. Plugin builds alias `react`, `react-dom`, `react-dom/client` and
// `react/jsx-runtime` (and `@tuff/panel-ui`, the kit in ./ui) to the shims in ./shims, which read this object.
import * as React from "react";
import * as ReactDom from "react-dom";
import * as ReactDomClient from "react-dom/client";
import * as JsxRuntime from "react/jsx-runtime";
import * as Ui from "./ui";

export const SHARED_KEY = "__tuffPanel";

export interface Shared {
  React: typeof React;
  ReactDom: typeof ReactDom;
  ReactDomClient: typeof ReactDomClient;
  JsxRuntime: typeof JsxRuntime;
  Ui: typeof Ui;
}

export function exposeShared(target: Record<string, unknown> = globalThis as Record<string, unknown>) {
  const shared: Shared = Object.freeze({ React, ReactDom, ReactDomClient, JsxRuntime, Ui });
  Object.defineProperty(target, SHARED_KEY, { value: shared, writable: false, configurable: false });
}
