import { defineConfig } from "vite";

export default defineConfig({
  // The built bundle is served from Yuyib's local custom-protocol origin.
  // Relative URLs keep it independent from the backend's URL spelling.
  base: "./",
  server: {
    host: "127.0.0.1",
    port: 4173,
  },
  build: {
    target: "es2022",
    outDir: "dist",
    assetsDir: "assets",
    emptyOutDir: true,
    sourcemap: true,
  },
});
