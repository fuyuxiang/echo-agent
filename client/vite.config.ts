/// <reference types="vitest" />
import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";
import path from "node:path";

// Tauri spawns a dev server it can then load into the webview.
// HMR works with the default Vite dev server; the fixed port keeps
// tauri.conf.json `devUrl` stable.
const HOST = "127.0.0.1";
const PORT = 1420;

// `process.env.VITEST` is set by vitest before vite evaluates the config
// file, so we can scope the monaco-editor alias to tests only and leave the
// production `dev` / `build` modes resolving the package through vite's
// normal `module`-field lookup.
const isVitest = String(process.env.VITEST) === "true";
const monacoEditorEntry = path.resolve(
  __dirname,
  "node_modules/monaco-editor/esm/vs/editor/editor.main.js",
);

// https://vitejs.dev/config/
export default defineConfig({
  plugins: [react()],
  resolve: {
    alias: [
      // Mirror EchoAgent's `@` alias so ported components resolve unchanged.
      { find: "@", replacement: path.resolve(__dirname, "src") },
      // Only needed in vitest: monaco-editor 0.52 ships no `exports`
      // field, so vite's bare-specifier resolver fails to find the entry.
      // Production builds resolve through the package's `module` field
      // (esm/vs/editor/editor.main.js), which also pulls in the five
      // built-in language contributions (basic-languages / css / html /
      // json / typescript). Tests pin the same file so behavior matches.
      // `monaco-editor/esm/...` deep paths are left untouched so vite's
      // `?worker` plugin can still process them.
      ...(isVitest
        ? [
            {
              find: /^monaco-editor$/,
              replacement: monacoEditorEntry,
            },
          ]
        : []),
    ],
  },
  // Tauri webview can't reach a host-relative absolute URL during dev
  // (no server origin), so always emit relative paths.
  base: "./",
  clearScreen: false,
  // HTML export loads on demand; prebundle its browser renderer before the
  // first export so discovering it cannot reload an active development page.
  optimizeDeps: { include: ["react-dom/server"] },
  server: {
    host: HOST,
    port: PORT,
    strictPort: true,
    // Tauri waits for this string before launching the webview.
    watch: {
      ignored: ["**/src-tauri/**", "**/vendor/theia-platform/**"],
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
          monaco: ["monaco-editor", "@monaco-editor/react"],
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
