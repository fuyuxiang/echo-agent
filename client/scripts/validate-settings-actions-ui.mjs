import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createUiReviewServer, reviewChromium, reviewOutput } from "./ui-review-runtime.mjs";
import { assertSettingsActionsSeparated } from "./ui-settings-audit.mjs";

assert.equal(process.argv.length, 2, "Run the complete settings action review");
assert.notEqual(process.env.UI_REVIEW_BASELINE, "1", "Validate the working tree, not the baseline");
const output = reviewOutput(mkdtempSync(join(tmpdir(), "echo-settings-actions-")));
const server = await createUiReviewServer(0);
const results = [];
writeFileSync(join(output, "settings-actions.json"), "[]\n");
let browser;
try {
  browser = await (await reviewChromium()).launch({ headless: true,
    ...(process.env.CHROME_BIN ? { executablePath: process.env.CHROME_BIN } : {}) });
  for (const populated of [false, true]) {
    for (const [width, height, theme, font] of [
      [1280, 800, "light", 13], [1280, 800, "dark", 13],
      [1024, 680, "light", 11], [1024, 680, "dark", 18],
      [1920, 1080, "light", 18], [1920, 1080, "dark", 11],
    ]) {
      const page = await browser.newPage({ viewport: { width, height }, colorScheme: theme });
      const errors = [];
      page.on("pageerror", error => errors.push(error.message));
      await page.addInitScript(size => {
        localStorage.setItem("echoagent.fontSize", String(size));
        const apply = () => {
          if (!document.documentElement) return false;
          document.documentElement.style.fontSize = `${16 * size / 13}px`;
          return true;
        };
        if (!apply()) {
          const observer = new MutationObserver(() => { if (apply()) observer.disconnect(); });
          observer.observe(document, { childList: true, subtree: true });
        }
      }, font);
      // The general UI fixture includes an organization session. Use the
      // existing model fixture to make empty/personal account state explicit.
      const parameters = new URLSearchParams({ surface: "model", theme,
        modelSettingsReview: "1", modelAccount: populated ? "personal" : "none" });
      await page.goto(`http://127.0.0.1:${server.httpServer.address().port}/__ui-review?${parameters}`);
      const geometry = await assertSettingsActionsSeparated(page);
      const rootFontSize = await page.evaluate(() => parseFloat(getComputedStyle(document.documentElement).fontSize));
      assert(Math.abs(rootFontSize - 16 * font / 13) < 0.001, "Requested settings font size must actually be applied");
      assert.equal(geometry.navigatedToConnections, populated, "Use the actual catalog-specific navigation path");
      assert.deepEqual(errors, [], "Settings browser runtime errors");
      await page.screenshot({ path: join(output, `settings-actions-${populated ? "populated" : "empty"}-${width}-${theme}-${font}.png`) });
      results.push({ populated, width, height, theme, font, rootFontSize, ...geometry });
      writeFileSync(join(output, "settings-actions.json"), JSON.stringify(results, null, 2));
      await page.close();
    }
  }
  assert.equal(results.length, 12, "Every catalog and layout combination must run");
  console.log(`Settings actions passed: ${results.length} empty/populated layouts, navigation, no overlap, corner click and cancel. Evidence: ${output}`);
} finally {
  await browser?.close();
  await server.close();
}
