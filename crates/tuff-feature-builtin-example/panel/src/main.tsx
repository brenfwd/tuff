import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import type { Mount } from "../../../web-ui/app/src/sdk/context";
import { GoldPage } from "./GoldPage";
import "./index.css";

export const mount: Mount = (element, ctx) => {
  const root = createRoot(element);
  root.render(
    <StrictMode>
      <GoldPage ctx={ctx} />
    </StrictMode>,
  );
  return () => queueMicrotask(() => root.unmount());
};
