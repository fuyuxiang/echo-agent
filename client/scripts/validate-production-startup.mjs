// Serve built assets only: a Vite dev server cannot reproduce chunk initialization failures.
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { existsSync } from "node:fs";
import { mkdir, mkdtemp, readFile, realpath, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { extname, join, resolve, sep } from "node:path";
import { reviewChromium } from "./ui-review-runtime.mjs";

assert.ok(process.argv.length <= 3, "Usage: node scripts/validate-production-startup.mjs [dist-directory]");
const output = process.env.PRODUCTION_SMOKE_OUTPUT
  ? resolve(process.env.PRODUCTION_SMOKE_OUTPUT)
  : await mkdtemp(join(tmpdir(), "echo-production-startup-"));
await mkdir(output, { recursive: true });
const report = { passed: false, pageErrors: [], consoleErrors: [], expectedNativeIpcErrors: [], resourceErrors: [] };
// The static browser intentionally has no native host. Keep this allowlist narrow;
// renderer/module failures and every other console error must fail the check.
const missingNativeIpc = [
  /^\[EchoAgent\] Failed to hydrate projects: TypeError: Cannot read properties of undefined \(reading 'invoke'\)/,
  /^list pending folder trust requests failed TypeError: Cannot read properties of undefined \(reading 'invoke'\)/,
];
const contentTypes = {
  ".html": "text/html; charset=utf-8", ".js": "text/javascript", ".mjs": "text/javascript",
  ".css": "text/css", ".json": "application/json", ".svg": "image/svg+xml", ".png": "image/png",
  ".jpg": "image/jpeg", ".jpeg": "image/jpeg", ".ico": "image/x-icon", ".wasm": "application/wasm",
  ".woff": "font/woff", ".woff2": "font/woff2", ".ttf": "font/ttf",
};
let server, browser, page;
function assertNoErrors() {
  assert.equal(report.pageErrors.length + report.consoleErrors.length + report.resourceErrors.length, 0,
    `Production startup reported errors: ${JSON.stringify(report)}`);
}

try {
  const root = await realpath(resolve(process.argv[2] || "dist"));
  report.dist = root;
  const html = await readFile(join(root, "index.html"), "utf8");
  assert.match(html, /<script\b[^>]*\btype=["']module["'][^>]*\bsrc=["'][^"']+\.js["']/,
    "Build the production assets with pnpm build before running this check");
  server = createServer(async (request, response) => {
    try {
      if (!["GET", "HEAD"].includes(request.method)) { response.writeHead(405).end(); return; }
      const pathname = decodeURIComponent(new URL(request.url, "http://localhost").pathname);
      if (pathname === "/favicon.ico") { response.writeHead(204).end(); return; }
      const file = await realpath(resolve(root, "." + (pathname === "/" ? "/index.html" : pathname)));
      if (!file.startsWith(root + sep)) { response.writeHead(403).end(); return; }
      const body = await readFile(file);
      response.writeHead(200, {
        "Content-Type": contentTypes[extname(file)] || "application/octet-stream",
        "Cache-Control": "no-store", "X-Content-Type-Options": "nosniff",
      }).end(request.method === "HEAD" ? undefined : body);
    } catch (error) {
      response.writeHead(error.code === "ENOENT" ? 404 : 500).end();
    }
  });
  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolve);
  });
  const chromium = await reviewChromium();
  const chrome = process.env.CHROME_BIN || (process.platform === "darwin" ? "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" : null);
  browser = await chromium.launch({ headless: true, ...(chrome && existsSync(chrome) ? { executablePath: chrome } : {}) });
  const context = await browser.newContext({ viewport: { width: 1280, height: 900 } });
  page = await context.newPage();
  page.setDefaultTimeout(20000);
  page.on("pageerror", error => report.pageErrors.push({ message: error.message, stack: error.stack }));
  page.on("console", message => {
    if (message.type() !== "error") return;
    const text = message.text();
    (missingNativeIpc.some(pattern => pattern.test(text)) ? report.expectedNativeIpcErrors : report.consoleErrors).push(text);
  });
  page.on("requestfailed", request => report.resourceErrors.push({ url: request.url(), error: request.failure()?.errorText }));
  page.on("response", response => {
    if (response.status() >= 400) report.resourceErrors.push({ url: response.url(), status: response.status() });
  });
  await page.goto(`http://127.0.0.1:${server.address().port}/`, { waitUntil: "load", timeout: 20000 });
  assertNoErrors();
  await page.waitForFunction(() => {
    const root = document.getElementById("root");
    const failure = [...(root?.querySelectorAll("h1") || [])]
      .find(heading => ["启动未完成", "界面启动失败"].includes(heading.textContent.trim()));
    if (failure) throw new Error(root.innerText);
    return !!root?.querySelector(".window-sidebar-brand") && !!root?.querySelector(".app__main");
  });
  await page.locator("#app-sidebar").waitFor({ state: "visible" });
  await page.locator(".app__main").waitFor({ state: "visible" });
  const brand = page.locator(".window-sidebar-brand");
  assert.equal(await brand.count(), 1, "Production shell must own exactly one window brand");
  assert.equal(await brand.innerText(), "EchoAgent");
  const toggle = page.locator(".window-sidebar-toggle");
  const anchor = await toggle.boundingBox();
  await toggle.click();
  await page.locator("#app-sidebar").waitFor({ state: "hidden" });
  assert.ok(await brand.isVisible(), "Collapsing the sidebar must retain the window brand");
  assert.deepEqual(await toggle.boundingBox(), anchor, "Production navigation must not move while collapsing");
  await toggle.click();
  await page.locator("#app-sidebar").waitFor({ state: "visible" });
  assert.deepEqual(await toggle.boundingBox(), anchor, "Production navigation must not move while expanding");
  await page.waitForLoadState("networkidle", { timeout: 10000 });
  assertNoErrors();
  report.passed = true;
} catch (error) {
  report.error = error.stack || String(error);
  process.exitCode = 1;
} finally {
  if (page && !page.isClosed()) {
    try {
      report.body = (await page.locator("body").innerText({ timeout: 5000 })).slice(0, 12000);
      await page.screenshot({ path: join(output, "startup.png"), timeout: 5000 });
    } catch (error) {
      report.captureError = String(error);
      report.passed = false;
      process.exitCode = 1;
    }
  }
  try { await browser?.close(); }
  finally {
    if (server?.listening) {
      server.closeAllConnections();
      await new Promise((resolve, reject) => server.close(error => error ? reject(error) : resolve()));
    }
    if (report.pageErrors.length + report.consoleErrors.length + report.resourceErrors.length > 0) {
      report.passed = false;
      report.error ||= "Unexpected browser errors occurred during production startup";
      process.exitCode = 1;
    }
    await writeFile(join(output, "report.json"), JSON.stringify(report, null, 2));
    console.log(JSON.stringify({ passed: report.passed, expectedNativeIpcErrors: report.expectedNativeIpcErrors.length, evidence: output, ...(report.error ? { error: report.error } : {}) }));
  }
}
