import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import type { Mount } from "../../../web-ui/app/src/sdk/context";
import { MapPage } from "./MapPage";
import "./index.css";

export const mount: Mount = (element, ctx) => {
  const root = createRoot(element);
  root.render(
    <StrictMode>
      <MapPage ctx={ctx} />
    </StrictMode>,
  );
  return () => queueMicrotask(() => root.unmount());
};
