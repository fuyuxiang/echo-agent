import assert from "node:assert/strict";
import test from "node:test";
import { createUiReviewServer, reviewChromium } from "./ui-review-runtime.mjs";
import { waitForVisualPolishSurface } from "./ui-review-readiness.mjs";

async function assertPending(readiness, message) {
  let timer;
  try {
    const settled = await Promise.race([
      readiness.then(() => true),
      new Promise(resolve => { timer = setTimeout(() => resolve(false), 100); }),
    ]);
    assert.equal(settled, false, message);
  } finally { clearTimeout(timer); }
}

test("visual sampling waits for real lazy pages and populated IPC lists", { timeout: 240000 }, async t => {
  const server = await createUiReviewServer(0);
  let browser;
  try {
    browser = await (await reviewChromium()).launch({ headless: true,
      ...(process.env.CHROME_BIN ? { executablePath: process.env.CHROME_BIN } : {}) });
    const scenarios = [
      { label: "技能", theme: "light", module: "experts-panel/index", command: "skills_list", content: ".sk-inst-row" },
      { label: "连接器", theme: "dark", module: "experts-panel/index", command: "mcp_list", content: ".mcp-item" },
    ];
    for (const scenario of scenarios) await t.test(scenario.label, async () => {
      const page = await browser.newPage({ viewport: { width: 1024, height: 680 }, colorScheme: scenario.theme });
      page.setDefaultNavigationTimeout(90000);
      page.setDefaultTimeout(15000);
      const errors = [];
      page.on("pageerror", error => errors.push(error.message));
      let releaseModule, readiness, navigation;
      const moduleGate = new Promise(resolve => { releaseModule = resolve; });
      try {
        await page.route(`**/src/components/${scenario.module}.tsx*`, async route => {
          await moduleGate;
          if (!page.isClosed()) await route.continue();
        });
        if (scenario.command) await page.addInitScript(command => {
          // Hold only the fixture's native list response; application modules
          // and presentation remain the real production implementation.
          let native;
          const gate = new Promise(resolve => { window.releaseReviewList = resolve; });
          Object.defineProperty(window, "__TAURI_INTERNALS__", {
            configurable: true,
            get: () => native,
            set(value) {
              const invoke = value.invoke;
              native = { ...value, invoke: async (...args) => {
                if (args[0] === command) { window.reviewListWaiting = true; await gate; }
                return invoke(...args);
              } };
            },
          });
        }, scenario.command);
        const params = new URLSearchParams({ surface: "page", label: scenario.label,
          theme: scenario.theme, extensionState: "populated" });
        navigation = page.goto(`http://127.0.0.1:${server.httpServer.address().port}/__ui-review?${params}`, { waitUntil: "domcontentloaded" });
        void navigation.catch(() => {});
        await page.locator('.placeholder-page[role="status"]').waitFor();
        readiness = waitForVisualPolishSurface(page, `page:${scenario.label}`).then(async () => {
          assert.equal(await page.locator('.placeholder-page[role="status"], .capability-loading').count(), 0,
            "A loading state must never become the comparison baseline");
          const count = await page.locator(scenario.content).count();
          assert(scenario.command ? count === 3 : count > 0, "The expected fixture content must be present");
        });
        void readiness.catch(() => {});
        await assertPending(readiness, "A mounted Suspense fallback must not count as a ready page");
        releaseModule();
        await navigation;
        if (scenario.command) {
          await page.locator(".capability-loading").waitFor();
          await page.waitForFunction(() => window.reviewListWaiting === true);
          await assertPending(readiness, "A mounted page must still wait for its IPC list");
          await page.evaluate(() => window.releaseReviewList());
        }
        await readiness;
        assert.deepEqual(errors, [], "Delayed loading must not cause browser errors");
      } finally {
        releaseModule();
        await page.close();
        await Promise.allSettled([readiness, navigation]);
      }
    });
  } finally { await browser?.close(); await server.close(); }
});
