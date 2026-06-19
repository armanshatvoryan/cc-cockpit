import { defineConfig } from "vite";
import solid from "vite-plugin-solid";

// Tauri expects a fixed dev port and a relative base so the bundled WebView can
// load assets from the app bundle.
export default defineConfig({
  plugins: [solid()],
  base: "./",
  clearScreen: false,
  server: { port: 1420, strictPort: true },
  build: { target: "esnext", outDir: "dist" },
});
