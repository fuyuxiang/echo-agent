import assert from "node:assert/strict";
import { writeFileSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { createUiReviewServer, reviewChromium, reviewOutput } from "./ui-review-runtime.mjs";
import { inspectEditorPaint } from "./editor-paint-audit.mjs";
const output = reviewOutput(join(tmpdir(), "echo-local-editor-paint"));
const auditOnly = process.argv.includes("--audit-only");
const server = await createUiReviewServer(0, { fixture: "/scripts/fixtures/enterprise-theme-review.tsx" });
const browser = await (await reviewChromium()).launch({ headless: true, executablePath: process.env.CHROME_BIN || "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" });
const evidence = [];
try {
  for (const [width, height, font] of [[1280, 800, 13], [1280, 800, 18], [1024, 680, 13], [1024, 680, 18]]) for (const theme of ["light", "dark"]) {
    const page = await browser.newPage({ viewport: { width, height }, colorScheme: theme });
    const errors = []; page.on("pageerror", error => errors.push(error.message));
    await page.goto(`http://127.0.0.1:${server.httpServer.address().port}/__ui-review?surface=monaco&theme=${theme}`, { waitUntil: "networkidle", timeout: 90000 });
    await page.evaluate(font => { document.documentElement.style.fontSize = `${16 * font / 13}px`; }, font);
    await page.locator(".monaco-diff-editor").waitFor({ timeout: 90000 });
    await page.waitForFunction(() => [...document.querySelectorAll(".view-lines span[class*=mtk]")].some(element => element.textContent.includes("保留")));
    const paint = await inspectEditorPaint(page, page, join(output, `monaco-${theme}-${width}-${font}-backdrop.png`), { diffHighlights: true });
    assert.deepEqual(paint.highlightFailures, [], "Changed characters must remain distinct from their changed line in both themes");
    await page.screenshot({ path: join(output, `monaco-${theme}-${width}-${font}.png`) });
    assert.deepEqual(errors, []);
    evidence.push({ width, height, font, theme, ...paint });
    if (width === 1280 && font === 13) {
      const collapsed = await page.addStyleTag({ content: ".monaco-diff-editor .char-insert,.monaco-diff-editor .char-delete{background:transparent!important;border-color:transparent!important}" });
      try {
        const negative = await inspectEditorPaint(page, page, join(output, `collapsed-negative-${theme}.png`), { diffHighlights: true });
        assert.equal(negative.highlightFailures.length, 2, "The audit must reject missing character highlights even when all text stays readable");
      } finally { await collapsed.evaluate(element => element.remove()); }
    }
    const switched = theme === "dark" ? "light" : "dark";
    await page.evaluate(theme => window.enterpriseThemeReview.setTheme(theme), switched);
    await page.waitForFunction(theme => document.documentElement.dataset.theme === theme, switched);
    await page.waitForFunction(expected => {
      const comment = [...document.querySelectorAll(".view-lines span[class*=mtk]")].find(element => element.textContent.trim().startsWith("//"));
      return comment && getComputedStyle(comment).color === expected;
    }, switched === "dark" ? "rgb(156, 184, 147)" : "rgb(78, 106, 88)");
    const live = await inspectEditorPaint(page, page, join(output, `monaco-live-switch-${switched}-${width}-${font}-backdrop.png`), { diffHighlights: true });
    assert.deepEqual(live.highlightFailures, [], "Live theme changes must preserve character highlights");
    await page.screenshot({ path: join(output, `monaco-live-switch-${switched}-${width}-${font}.png`) });
    evidence.push({ width, height, font, theme: switched, state: "live-switch", ...live });
    writeFileSync(join(output, "evidence.json"), JSON.stringify(evidence, null, 2));
    await page.close();
  }
  const failures = evidence.flatMap(record => record.failures.map(failure => ({ width: record.width, font: record.font, theme: record.theme, ...failure })));
  writeFileSync(join(output, "failures.json"), JSON.stringify(failures, null, 2));
  console.log(JSON.stringify({ states: evidence.length, pixels: evidence.reduce((sum, record) => sum + record.pixels, 0), failures: failures.length, output }));
  if (!auditOnly) assert.deepEqual(failures, [], "Actual editor backdrop contrast failures");
} finally { await browser.close(); await server.close(); }
