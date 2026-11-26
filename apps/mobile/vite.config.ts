import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
// @ts-expect-error type error without @types/node package
import process from "node:process";

// On a phone, `tauri ios dev` serves the UI to the device over the network.
const host = process.env.TAURI_DEV_HOST;

export default defineConfig(() => ({
  root: __dirname,
  plugins: [react()],
  clearScreen: false,
  build: { outDir: "dist", emptyOutDir: true },
  server: {
    port: 1430,
    strictPort: true,
    host: host || false,
    hmr: host ? { protocol: "ws", host, port: 1431 } : undefined,
    // The design tokens and types come from the desktop app's src/.
    fs: { allow: ["../.."] },
    watch: { ignored: ["**/src-tauri/**"] },
  },
}));
