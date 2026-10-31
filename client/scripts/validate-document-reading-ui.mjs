import assert from "node:assert/strict";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createUiReviewServer, reviewChromium } from "./ui-review-runtime.mjs";

const output = mkdtempSync(join(tmpdir(), "echo-document-ui-"));
const server = await createUiReviewServer(0, { fixture: "/scripts/fixtures/document-reading-review.tsx" });
let browser;
try {
  browser = await (await reviewChromium()).launch({ headless: true, executablePath: process.env.CHROME_BIN || "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" });
  const page = await browser.newPage();
  const errors = []; page.on("pageerror", e => errors.push(e.message));
  const port = server.httpServer.address().port;
  for (const width of [420, 900]) for (const theme of ["light", "dark"]) {
    await page.setViewportSize({ width, height: 720 });
    await page.goto(`http://127.0.0.1:${port}/__ui-review?theme=${theme}`);
    const header = page.locator(".execution-process__header");
    await header.click();
    assert.match(await page.locator(".execution-process__metrics").textContent(), /决赛准备3-评委参考问题.xlsx · 已读取 1,240\/4,001 段内容/);
    assert.equal(await header.getAttribute("aria-expanded"), "false");
    await header.click();
    const button = page.getByRole("button", { name: /已读取 1,240 \/ 4,001 段内容/ });
    await button.click();
    const status = page.getByRole("status", { name: "资料读取进度" });
    await status.waitFor();
    assert.match(await status.textContent(), /已保存 12 份阶段记录/);
    assert.equal(await page.getByRole("progressbar").getAttribute("value"), "1240");
    assert.match(await status.textContent(), /本次范围：今年选手/);
    await page.getByRole("button", { name: "查看阶段记录" }).click();
    assert.match(await page.getByRole("note").textContent(), /document-findings.md/);
    const layout = await status.evaluate(el => ({ width: el.clientWidth, scroll: el.scrollWidth, left: el.getBoundingClientRect().left, right: el.getBoundingClientRect().right }));
    assert.ok(layout.scroll <= layout.width + 1 && layout.left >= 0 && layout.right <= width);
    await page.screenshot({ path: join(output, `document-${width}-${theme}.png`) });
    await button.focus(); await page.keyboard.press("Enter");
    assert.equal(await status.count(), 0);
  }
  assert.deepEqual(errors, []);
  console.log(`Document reading UI: 4 layouts and keyboard interactions passed. Screenshots: ${output}`);
} finally { await browser?.close(); await server.close(); }
