import assert from "node:assert/strict";
import { writeFileSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { createUiReviewServer, reviewChromium, reviewOutput } from "./ui-review-runtime.mjs";

const output = reviewOutput(join(tmpdir(), "echo-theme-lifecycle"));
const server = await createUiReviewServer(0, { fixture: "/scripts/fixtures/enterprise-theme-review.tsx" });
const evidence = [];
let browser, page;
try {
  browser = await (await reviewChromium()).launch({ headless: true,
    ...(process.env.CHROME_BIN ? { executablePath: process.env.CHROME_BIN } : {}) });
  page = await browser.newPage({ colorScheme: "light", viewport: { width: 1024, height: 680 } });
  page.setDefaultNavigationTimeout(90000);
  const errors = []; page.on("pageerror", error => errors.push(error.message));
  await page.addInitScript(() => {
    const observer = new MutationObserver(() => {
      if (document.querySelector("#root > *")) {
        window.themeAtMount = { theme: document.documentElement.dataset.theme, scheme: document.documentElement.style.colorScheme };
        observer.disconnect();
      }
    });
    observer.observe(document, { childList: true, subtree: true });
  });
  await page.goto(`http://127.0.0.1:${server.httpServer.address().port}/__ui-review?surface=skill-import&theme=light`, { waitUntil: "networkidle" });
  await page.getByRole("button", { name: "选择 Markdown 或 ZIP 技能文件", exact: true }).click();
  await page.locator(".sk-risk--medium").first().waitFor();
  await page.evaluate(() => { window.portalIdentity = document.querySelector(".sk-import"); });
  const inspect = async (name, theme, preference, mounted = true) => {
    await page.waitForFunction(theme => document.documentElement.dataset.theme === theme, theme);
    const result = await page.evaluate(() => ({ theme: document.documentElement.dataset.theme, scheme: document.documentElement.style.colorScheme, preference: localStorage.getItem("echoagent.theme"), samePortal: window.portalIdentity === document.querySelector(".sk-import"), rows: document.querySelectorAll(".sk-batch-row").length, firstMount: window.themeAtMount }));
    assert.equal(result.scheme, theme); assert.equal(result.preference, preference);
    if (mounted) { assert(result.samePortal, "Theme changes must retain the existing portal"); assert.equal(result.rows, 2); }
    evidence.push({ name, ...result });
    await page.screenshot({ path: join(output, `${name}.png`) });
  };
  for (const theme of ["dark", "light"]) {
    await page.evaluate(theme => window.enterpriseThemeReview.setTheme(theme), theme);
    await inspect(`explicit-${theme}`, theme, theme);
  }
  await page.evaluate(() => window.enterpriseThemeReview.setTheme("system"));
  await inspect("system-light", "light", "system");
  await page.emulateMedia({ colorScheme: "dark" });
  await inspect("system-dark", "dark", "system");
  // The restore flag leaves persisted preferences untouched before initializeTheme.
  await page.goto(page.url() + "&restore=1", { waitUntil: "networkidle" });
  await inspect("restored-system-dark", "dark", "system", false);
  assert.deepEqual(evidence.at(-1).firstMount, { theme: "dark", scheme: "dark" });
  await page.evaluate(() => window.enterpriseThemeReview.setTheme("light"));
  await page.reload({ waitUntil: "networkidle" });
  await inspect("restored-explicit-light", "light", "light", false);
  assert.deepEqual(evidence.at(-1).firstMount, { theme: "light", scheme: "light" });
  await page.goto(`http://127.0.0.1:${server.httpServer.address().port}/__ui-review?surface=monaco&theme=light`, { waitUntil: "networkidle" });
  await page.locator(".monaco-diff-editor").waitFor({ timeout: 90000 });
  for (const theme of ["dark", "light"]) {
    await page.evaluate(() => window.enterpriseThemeReview.setEditorOpen(false));
    await page.locator(".monaco-diff-editor").waitFor({ state: "hidden" });
    await page.waitForFunction(async () => await window.enterpriseThemeReview.editorModelCount() === 0);
    await page.evaluate(theme => window.enterpriseThemeReview.setTheme(theme), theme);
    await page.waitForFunction(theme => document.documentElement.dataset.theme === theme, theme);
    await page.evaluate(() => window.enterpriseThemeReview.setEditorOpen(true));
    await page.locator(".monaco-diff-editor").waitFor();
    assert.equal(await page.evaluate(() => window.enterpriseThemeReview.editorModelCount()), 2, "Only this review's two read-only models may remain");
    await page.waitForFunction(expected => {
      const comment = [...document.querySelectorAll(".view-lines span[class*=mtk]")].find(element => element.textContent.trim().startsWith("//"));
      return comment && getComputedStyle(comment).color === expected;
    }, theme === "dark" ? "rgb(156, 184, 147)" : "rgb(78, 106, 88)");
    await page.locator(".monaco-diff-editor .line-insert").first().waitFor();
    const file = `monaco-reopened-${theme}.png`;
    await page.screenshot({ path: join(output, file) });
    evidence.push({ name: `monaco-reopened-${theme}`, theme, file });
  }
  assert.deepEqual(errors, []);
  writeFileSync(join(output, "evidence.json"), JSON.stringify(evidence, null, 2));
  console.log(JSON.stringify({ passed: true, states: evidence.length, output }));
} catch (error) {
  if (page) await page.screenshot({ path: join(output, "failure.png") }).catch(() => {});
  writeFileSync(join(output, "partial-evidence.json"), JSON.stringify({ error: String(error), evidence }, null, 2));
  throw error;
} finally { await browser?.close(); await server.close(); }
