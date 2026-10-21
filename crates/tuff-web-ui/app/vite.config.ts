import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { viteSingleFile } from "vite-plugin-singlefile";

export default defineConfig({
  base: "/admin/",
  plugins: [react(), tailwindcss(), viteSingleFile()],
  build: {
    outDir: process.env.TUFF_WEB_OUT ?? "dist",
    emptyOutDir: true,
    modulePreload: false,
    reportCompressedSize: false,
  },
});
