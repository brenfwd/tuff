// Plugin builds resolve `react-dom/client` here: the shell's ReactDOM.
import type { Shared } from "../shared";

const client = (globalThis as unknown as { __tuffPanel: Shared }).__tuffPanel.ReactDomClient;

export const { createRoot, hydrateRoot } = client;
