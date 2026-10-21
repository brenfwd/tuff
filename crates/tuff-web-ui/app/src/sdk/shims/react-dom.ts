// Plugin builds resolve `react-dom` here: the shell's ReactDOM.
import type { Shared } from "../shared";

const ReactDom = (globalThis as unknown as { __tuffPanel: Shared }).__tuffPanel.ReactDom;

export default ReactDom;
export const { createPortal, flushSync, version } = ReactDom;
