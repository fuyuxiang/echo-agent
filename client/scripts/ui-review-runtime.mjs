import { existsSync, mkdirSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { createServer } from "vite";

// Reuse the optional Theia browser test installation, or an explicitly supplied
// Playwright module. Browser tooling is not shipped in the application bundle.
export async function reviewChromium() {
  const candidate = process.env.PLAYWRIGHT_MODULE || resolve("vendor/theia-platform/node_modules/playwright/index.mjs");
  if (existsSync(candidate)) return (await import(pathToFileURL(candidate).href)).chromium;
  try { return (await import("playwright")).chromium; }
  catch { throw new Error("Set PLAYWRIGHT_MODULE to an installed Playwright index.mjs, or prepare the Theia browser test dependencies."); }
}

export function reviewOutput(fallback) {
  const output = process.env.UI_REVIEW_OUTPUT || fallback;
  mkdirSync(output, { recursive: true });
  return output;
}

export async function createUiReviewServer(port = 1439) {
  const baseline = process.env.UI_REVIEW_BASELINE === "1";
  const originalSources = new Map();
  const server = await createServer({
    server: { host: "127.0.0.1", port, strictPort: true },
    plugins: [{ name: "ui-review-original-source", enforce: "pre", load(id) {
      const path = id.split("?")[0];
      if (!baseline || !/\.(tsx?|css)$/.test(path) || /[?&](raw|url|worker)([=&]|$)/.test(id) || !path.startsWith(resolve("src") + "/")) return null;
      const relative = path.slice(process.cwd().length + 1);
      if (!originalSources.has(relative)) {
        try { originalSources.set(relative, execFileSync("git", ["show", `HEAD:${relative}`], { encoding: "utf8", maxBuffer: 8 * 1024 * 1024, stdio: ["ignore", "pipe", "ignore"] })); }
        catch { originalSources.set(relative, null); }
      }
      return originalSources.get(relative);
    } }, { name: "isolated-ui-review", configureServer(server) {
      server.middlewares.use((request, response, next) => {
        if (!request.url?.startsWith("/__ui-review")) return next();
        const html = `<!doctype html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><style>*{box-sizing:border-box}*,*::before,*::after{animation:none!important;transition:none!important}html,body,#root{width:100%;height:100%;margin:0}body{overflow:hidden;background:var(--echo-bg-secondary);font:13px -apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif}</style></head><body><div id="root"></div><script type="module" src="/scripts/fixtures/ui-surfaces.tsx"></script></body></html>`;
        void server.transformIndexHtml(request.url, html).then(result => {
          response.setHeader("Content-Type", "text/html"); response.end(result);
        }).catch(next);
      });
    } }],
  });
  await server.listen();
  return server;
}
