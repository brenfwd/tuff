import "@fontsource-variable/inter";
import "@fontsource-variable/space-grotesk";
import "@fontsource-variable/jetbrains-mono";
import "./styles.css";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import App from "./App";

// Static snapshots live at /docs/<slug>/ and /blog/<slug>/; hand them over to the hash router.
const staticPage = /^\/(docs|blog)(\/.+?)?\/?$/.exec(location.pathname.slice(import.meta.env.BASE_URL.length - 1));
if (staticPage && !location.hash.startsWith("#/")) {
  const section = location.hash.slice(1);
  history.replaceState(null, "", `${import.meta.env.BASE_URL}#/${staticPage[1]}${staticPage[2] ?? ""}${section ? `?h=${encodeURIComponent(section)}` : ""}`);
}

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
