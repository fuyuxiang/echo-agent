import assert from "node:assert/strict";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { inspectEditorPaint } from "./editor-paint-audit.mjs";
import { reviewChromium } from "./ui-review-tools.mjs";

test("syntax contrast ignores repainted editor chrome and rejects unreadable visible glyphs", { timeout: 60000 }, async () => {
  const output = mkdtempSync(join(tmpdir(), "echo-editor-paint-test-"));
  let browser;
  try {
    browser = await (await reviewChromium()).launch({ headless: true,
      ...(process.env.CHROME_BIN ? { executablePath: process.env.CHROME_BIN } : {}) });
    const page = await browser.newPage({ viewport: { width: 800, height: 600 } });
    const tokens = ["visible-token", "export", "const", "status", "completed", "retries", "number", "console", "// 保留可见语法检查"];
    await page.setContent(`<style>
      body { margin: 0; background: white; }
      .monaco-editor { position: relative; width: 760px; height: 240px; background: white; color: black; font: 16px/24px monospace; }
      .view-lines { position: absolute; inset: 0; }
      .view-lines > span { display: block; width: max-content; }
      .view-lines > .covered { position: absolute; top: 0; width: 60px; overflow: hidden; white-space: nowrap; color: rgb(163, 21, 21); }
      .scrollbar, .minimap { position: absolute; top: 0; width: 60px; height: 24px; z-index: 2; background: rgb(147, 147, 147); }
    </style><div class="monaco-editor"><div class="view-lines">
      ${tokens.map(text => `<span class="mtk1">${text}</span>`).join("")}
      <span class="mtk2 covered" style="left:300px">covered-scrollbar</span>
      <span class="mtk2 covered" style="left:400px">covered-minimap</span>
    </div><div class="scrollbar" style="left:300px"></div><div class="minimap" style="left:400px"></div></div>`);
    await page.evaluate(() => {
      // Deterministically reproduce a scrollbar/minimap repaint between the
      // ink and backdrop screenshots, while the underlying token stays hidden.
      const observer = new MutationObserver(records => {
        if (records.some(record => [...record.addedNodes].some(node => node.textContent?.includes("-webkit-text-fill-color")))) {
          for (const chrome of document.querySelectorAll(".scrollbar, .minimap")) chrome.style.backgroundColor = "rgb(179, 179, 179)";
        }
      });
      observer.observe(document.head, { childList: true });
    });
    const paint = await inspectEditorPaint(page, page, join(output, "readable.png"));
    assert.deepEqual(paint.failures, [], "Obscured tokens must not inherit the editor chrome's contrast");
    assert(paint.samples.every(sample => !sample.text.startsWith("covered-")), "Only visible glyph pixels may be sampled");
    assert.equal(paint.samples.length, tokens.length, "Visible syntax must still participate in the audit");

    await page.locator(".mtk1").first().evaluate(element => { element.style.color = "rgb(220, 220, 220)"; });
    const unreadable = await inspectEditorPaint(page, page, join(output, "unreadable.png"));
    assert(unreadable.failures.some(sample => sample.text === "visible-token" && sample.minimum < 4.5),
      "The occlusion fix must still reject low-contrast visible syntax");
  } finally { await browser?.close(); rmSync(output, { recursive: true, force: true }); }
});
