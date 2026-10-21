import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

export default defineConfig({
  base: "/",
  plugins: [react(), tailwindcss()],
  build: {
    target: "es2022",
    cssCodeSplit: true,
    rollupOptions: {
      output: {
        manualChunks: (id: string) => (/node_modules\/(react|react-dom|scheduler|wouter)\//.test(id) ? "react" : undefined),
      },
    },
  },
});
