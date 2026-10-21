import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { panelPage } from "../../tuff-web-ui/app/sdk/vite";

export default defineConfig({
  plugins: [react(), tailwindcss()],
  ...panelPage({ entry: "src/main.tsx", name: "gold" }),
});
