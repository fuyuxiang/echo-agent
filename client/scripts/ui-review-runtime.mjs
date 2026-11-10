import { mkdirSync } from "node:fs";
import { join } from "node:path";
import { createServer } from "vite";
import { createUiReviewBaseline } from "./ui-review-baseline.mjs";
import { createUiReviewCache } from "./ui-review-cache.mjs";
export { reviewChromium } from "./ui-review-tools.mjs";

// Locked third-party entry points used by the real fixtures. Finish this batch
// before opening a page: optimizer reloads can otherwise dismiss an active
// dialog halfway through an acceptance action. Application code is never
// prebundled here, so HEAD source/CSS isolation remains intact.
const reviewDependencies = [
  "react", "react/jsx-runtime", "react/jsx-dev-runtime", "react-dom", "react-dom/client", "react-dom/server",
  "zustand", "lucide-react", "@tauri-apps/api/core", "@tauri-apps/api/event", "@tauri-apps/api/window",
  "@tauri-apps/api/webview", "@tauri-apps/plugin-dialog", "qrcode", "react-markdown", "remark-gfm",
  "remark-breaks", "remark-math", "rehype-highlight", "rehype-katex", "rehype-sanitize", "lowlight",
  "jszip", "exceljs", "docx", "@pdf-lib/fontkit", "pdf-lib", "pptxgenjs", "@xterm/xterm", "@xterm/addon-fit",
  "linkify-it", "marked", "unist-util-visit", "katex", "mermaid", "@monaco-editor/react", "monaco-editor",
  "@tiptap/react", "@tiptap/starter-kit", "@tiptap/extension-table", "@tiptap/extension-text-align",
];

export function reviewOutput(fallback) {
  const output = process.env.UI_REVIEW_OUTPUT || fallback;
  mkdirSync(output, { recursive: true });
  return output;
}

export function annotateUiReviewFailure(error) {
  if (process.env.GITHUB_ACTIONS !== "true") return;
  const detail = String(error?.message ?? error).slice(0, 3500)
    .replaceAll("%", "%25").replaceAll("\r", "%0D").replaceAll("\n", "%0A");
  console.error(`::error::${detail}`);
}

export async function createUiReviewServer(port = 1439, {
  freezeMotion = true,
  fixture = "/scripts/fixtures/ui-surfaces.tsx",
  baseline = process.env.UI_REVIEW_BASELINE === "1",
  currentFixtures = false,
  baselineRevision,
  cacheRoot,
} = {}) {
  const original = baseline ? createUiReviewBaseline(process.cwd(), { currentFixtures, revision: baselineRevision }) : null;
  const cache = original ? null : createUiReviewCache(fixture, cacheRoot);
  const dispose = () => { try { cache?.close(); } finally { original?.close(); } };
  let server;
  try { server = await createServer({
    // Baseline and current pages may run together; their dependency optimizers
    // must not invalidate each other's module URLs or share a cache lifecycle.
    cacheDir: original ? join(original.directory, ".vite") : cache.directory,
    optimizeDeps: { holdUntilCrawlEnd: false, entries: [], noDiscovery: true, include: reviewDependencies },
    // Review pages navigate rapidly and deliberately abort some entry scripts.
    // Only transform requested modules: speculative warmups can outlive the
    // browser and leave Vite 5 waiting indefinitely during server.close().
    server: { host: "127.0.0.1", port, strictPort: true, preTransformRequests: false,
      ...(original ? { fs: { allow: [process.cwd(), original.directory] } } : {}),
    },
    plugins: [...(original ? [original.plugin] : []), { name: "isolated-ui-review", configureServer(server) {
      server.middlewares.use((request, response, next) => {
        if (!request.url?.startsWith("/__ui-review")) return next();
        // Screenshot fixtures can freeze motion; rendering regressions must
        // exercise production transitions and prefers-reduced-motion instead.
        const motionCss = freezeMotion ? "*,*::before,*::after{animation:none!important;transition:none!important}" : "";
        const html = `<!doctype html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><style>*{box-sizing:border-box}${motionCss}html,body,#root{width:100%;height:100%;margin:0}body{overflow:hidden;color:var(--echo-text-strong);background:var(--echo-bg-secondary);font:13px -apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif}</style></head><body><div id="root"></div><script type="module" src="${fixture}"></script></body></html>`;
        void server.transformIndexHtml(request.url, html).then(result => {
          response.setHeader("Content-Type", "text/html"); response.end(result);
        }).catch(next);
      });
    } }],
  }); } catch (error) { dispose(); throw error; }
  try {
    const optimizer = server.environments.client.depsOptimizer;
    await optimizer?.init();
    if (optimizer) await Promise.all(Object.values(optimizer.metadata.discovered).map(dependency => dependency.processing));
    if (port === 0) {
      // Vite 5 treats a configured zero as its default port. Listen on the
      // underlying server instead so the OS allocates and holds an ephemeral
      // port, without the race of probing an unused port and reopening it.
      await new Promise((resolve, reject) => {
        const http = server.httpServer;
        http.once("error", reject);
        http.listen(0, "127.0.0.1", () => {
          http.removeListener("error", reject);
          resolve();
        });
      });
    } else {
      await server.listen();
    }
  } catch (error) {
    try { await server.close(); }
    catch (closeError) { throw new AggregateError([error, closeError], "UI review startup and shutdown failed"); }
    finally { dispose(); }
    throw error;
  }
  const close = server.close.bind(server);
  let closing;
  server.close = () => closing ??= (async () => {
    try {
      // Vite 6 owns cancellation of its dependency crawl and pending transforms.
      // Waiting for crawl idle first can remain pending forever after aborted
      // navigation, hiding both successful results and the original assertion.
      const stopped = close();
      // The browser has already closed when callers dispose a review server.
      // Drain keep-alive/aborted requests too, so Node does not keep a fixture
      // listener alive after all assertions have completed.
      server.httpServer?.closeAllConnections?.();
      await stopped;
    } finally { dispose(); }
  })();
  return server;
}
