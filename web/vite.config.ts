import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

export default defineConfig({
  plugins: [react(), tailwindcss()],
  server: {
    port: 5173,
    proxy: {
      "/api": "http://localhost:58123",
      "/ws": {
        target: "ws://localhost:58123",
        ws: true,
      },
    },
  },
  build: {
    // Gateway-driven builds set ECHO_DASHBOARD_OUT_DIR so vite writes to
    // ``dist.staging``; the gateway then atomically swaps the staging result
    // into ``dist`` after validating it. The fallback (``dist``) keeps a
    // plain ``pnpm build`` outside the gateway writing where users expect.
    // ``emptyOutDir`` still wipes whatever was at the configured outDir the
    // moment the build starts — which is why the gateway uses staging as its
    // target rather than dist directly. See echo_agent/gateway/dashboard_build.py.
    outDir: process.env.ECHO_DASHBOARD_OUT_DIR ?? "dist",
    emptyOutDir: true,
  },
  test: {
    environment: "jsdom",
    globals: true,
    setupFiles: ["./src/test/setup.ts"],
  },
});
