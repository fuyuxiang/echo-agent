/// <reference types="vitest" />
import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";
import path from "node:path";

// Tauri spawns a dev server it can then load into the webview.
// HMR works with the default Vite dev server; the fixed port keeps
// tauri.conf.json `devUrl` stable.
const HOST = "127.0.0.1";
const PORT = 1420;

// https://vitejs.dev/config/
export default defineConfig({
  plugins: [react()],
  resolve: {
    alias: [
      // The package's development condition points at unpublished TS sources.
      // Always bundle its shipped ESM build in dev, tests, and production.
      { find: /^@apollo-design\/docx-preview$/, replacement: path.resolve(__dirname, "node_modules/@apollo-design/docx-preview/dist/docx-preview.mjs") },
      // Mirror EchoAgent's `@` alias so ported components resolve unchanged.
      { find: "@", replacement: path.resolve(__dirname, "src") },
    ],
  },
  // Tauri webview can't reach a host-relative absolute URL during dev
  // (no server origin), so always emit relative paths.
  base: "./",
  clearScreen: false,
  // Prebundle on-demand document engines so opening/exporting a file cannot
  // trigger dependency discovery and reload an active development editor.
  optimizeDeps: { include: [
    "react-dom/server", "@office-kit/pptx", "@office-kit/pptx-preview",
    "@apollo-design/docx-preview", "pdfjs-dist/legacy/build/pdf.mjs", "ssf", "exceljs", "jszip",
  ] },
  server: {
    host: HOST,
    port: PORT,
    strictPort: true,
    // Tauri waits for this string before launching the webview.
    watch: {
      ignored: ["**/src-tauri/**"],
    },
  },
  // Produce asset URLs that work from the tauri:// or file:// origin
  // the production webview uses.
  build: {
    target: "es2021",
    // Split the heavy markdown / syntax-highlight libs out of the app
    // chunk so the main bundle stays small and fast to hot-reload.
    rollupOptions: {
      output: {
        manualChunks: {
          markdown: [
            "react-markdown",
            "remark-gfm",
            "remark-breaks",
            "remark-math",
            "rehype-highlight",
            "rehype-sanitize",
            "lowlight",
            // Keep math rendering with its shared Markdown dependencies so
            // splitting them cannot create a startup initialization cycle.
            "katex",
            "rehype-katex",
          ],
          mermaid: ["mermaid"],
        },
      },
    },
  },
  test: {
    maxWorkers: 2,
    coverage: {
      provider: "v8",
      reportOnFailure: true,
      // Include unimported production modules too; experiments stay in the
      // denominator and are reported separately in the audit.
      include: ["src/**/*.{ts,tsx}"],
      exclude: ["src/**/*.d.ts", "src/**/*.{test,spec}.{ts,tsx}", "src/test-setup.ts"],
      reporter: ["text-summary", "json-summary", "json", "html"],
      reportsDirectory: process.env.AUDIT_COVERAGE_DIR || "coverage",
    },
    projects: [
      {
        extends: true,
        test: {
          name: "ui",
          include: ["src/**/*.{test,spec}.{ts,tsx}"],
          environment: "jsdom",
          setupFiles: ["./src/test-setup.ts"],
        },
      },
      {
        extends: true,
        test: {
          name: "scripts",
          include: ["scripts/**/*.test.mjs"],
          environment: "node",
          setupFiles: [],
        },
      },
    ],
    globals: true,
    // Ordinary styles stay stubbed; portable HTML must test the real CSS text.
    css: { include: [/\.css\?raw$/] },
  },
});
