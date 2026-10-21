import "@fontsource-variable/inter";
import "@fontsource-variable/space-grotesk";
import "@fontsource-variable/jetbrains-mono";
import "./styles.css";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import App from "./App";

// Static snapshots live at /docs/<slug>/; hand them over to the hash router.
const staticDoc = /\/docs\/(.+?)\/?$/.exec(location.pathname.slice(import.meta.env.BASE_URL.length - 1));
if (staticDoc && !location.hash) history.replaceState(null, "", `${import.meta.env.BASE_URL}#/docs/${staticDoc[1]}`);

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
