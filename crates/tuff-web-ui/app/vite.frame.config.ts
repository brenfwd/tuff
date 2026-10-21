import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { viteSingleFile } from "vite-plugin-singlefile";

export default defineConfig({
  base: "./",
  plugins: [react(), tailwindcss(), viteSingleFile()],
  build: {
    outDir: process.env.TUFF_WEB_OUT ?? "dist",
    emptyOutDir: false,
    modulePreload: false,
    reportCompressedSize: false,
    rollupOptions: { input: "frame.html" },
  },
});
