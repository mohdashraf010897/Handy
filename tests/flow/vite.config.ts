// Vite server for the isolated Per-app tone UI harness (see
// playwright.flow.config.ts). Port 1520 keeps it clear of the app's 1420.
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { resolve } from "path";

const repoRoot = resolve(__dirname, "../..");

export default defineConfig({
  root: __dirname,
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: {
      "@": resolve(repoRoot, "src"),
      "@/bindings": resolve(repoRoot, "src/bindings.ts"),
    },
  },
  clearScreen: false,
  server: {
    port: 1520,
    strictPort: true,
    watch: { ignored: ["**/src-tauri/**"] },
  },
});
