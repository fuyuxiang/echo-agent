import assert from "node:assert/strict";
import { readFileSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { createUiReviewServer, reviewChromium, reviewOutput } from "./ui-review-runtime.mjs";

const output = reviewOutput(resolve("src-tauri/target/share-export-review"));
const server = await createUiReviewServer(0, { fixture: "/scripts/fixtures/share-export-review.tsx" });
const url = `http://127.0.0.1:${server.httpServer.address().port}/__ui-review`;
const evidence = [];
let browser;
try {
  browser = await (await reviewChromium()).launch({ headless: true, executablePath: process.env.CHROME_BIN || "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" });
  for (const theme of ["light", "dark"]) {
    const page = await browser.newPage({ viewport: { width: 1280, height: 900 } });
    page.setDefaultNavigationTimeout(60000);
    const errors = [];
    page.on("pageerror", error => errors.push(error.message));
    await page.goto(`${url}?theme=${theme}`);
    await page.getByText("正文结束", { exact: true }).waitFor();
    const clientStyle = await page.locator(".hljs-keyword").first().evaluate(el => getComputedStyle(el).color);
    await page.getByRole("button", { name: "导出与本地分享", exact: true }).click();
    await page.getByRole("button", { name: "导出 HTML", exact: true }).click();
    await page.waitForFunction(() => window.exportReview?.saves.length === 1, null, { timeout: 60000 });
    const saved = await page.evaluate(() => window.exportReview.saves[0]);
    assert.equal(saved.extension, "html");
    assert.match(saved.suggestedName, /\.html$/);
    assert.doesNotMatch(saved.content, /默认隐藏的内部过程/);
    assert.match(saved.content, /data:font\/woff2;base64,/);
    const file = join(output, `会话导出-${theme}.html`);
    writeFileSync(file, saved.content);
    await page.getByRole("dialog").waitFor({ state: "hidden" });
    assert.equal(await page.evaluate(() => window.exportReview.notices.length), 1);
    // The saved file must be independent of the application, its server and network.
    const offline = await browser.newContext({ offline: true });
    try {
      for (const width of [1280, 768, 350]) {
        const reading = await offline.newPage();
        await reading.setViewportSize({ width, height: 900 });
        const requests = [];
        const readingErrors = [];
        reading.on("request", request => { if (/^https?:/.test(request.url())) requests.push(request.url()); });
        reading.on("pageerror", error => readingErrors.push(error.message));
        await reading.goto(pathToFileURL(file).href);
        await reading.evaluate(() => document.fonts.ready);
        assert.equal(await reading.title(), "会话导出 · 阅读效果验收");
        assert.equal(await reading.locator("main h1").count(), 2);
        assert.equal(await reading.locator("table").count(), 2);
        assert.equal(await reading.locator(".katex").count(), 3);
        assert.equal(await reading.locator(".md-export-diagram img").count(), 2);
        assert.equal(await reading.locator('input[type="checkbox"][checked]').count(), 1);
        assert.equal(await reading.locator("script,button,iframe,object").count(), 0);
        assert.equal(await reading.evaluate(() => window.pwned), undefined);
        assert.equal(await reading.locator(".hljs-keyword").first().evaluate(el => getComputedStyle(el).color), clientStyle);
        assert.equal(await reading.locator(".md-export-fallback").count(), 0);
        assert.ok(await reading.locator("img").evaluateAll(images => images.every(image => image.complete && image.naturalWidth > 0)), "inline images and diagrams decode offline");
        const geometry = await reading.evaluate(() => ({
          width: innerWidth, scroll: document.documentElement.scrollWidth,
          tables: [...document.querySelectorAll(".md-table-wrapper")].map(el => ({ client: el.clientWidth, scroll: el.scrollWidth })),
          codeScroll: [...document.querySelectorAll("pre code")].some(el => el.scrollWidth > el.clientWidth),
          mathFontLoaded: document.fonts.check('16px KaTeX_Main'),
        }));
        assert.ok(geometry.scroll <= width + 1, JSON.stringify(geometry));
        assert.ok(geometry.tables[1].scroll > geometry.tables[1].client, "wide table scrolls within its wrapper");
        assert.ok(geometry.codeScroll, "long code keeps a local scroll area");
        assert.equal(geometry.mathFontLoaded, true);
        assert.deepEqual(requests, []);
        assert.deepEqual(readingErrors, []);
        await reading.screenshot({ path: join(output, `reading-${theme}-${width}.png`), fullPage: true });
        evidence.push({ theme, width, bytes: Buffer.byteLength(saved.content), geometry });
        await reading.close();
      }
    } finally { await offline.close(); }
    assert.deepEqual(errors, []);
    await page.close();
  }
  // Cancel a native save: no success notice, menu remains usable and process remains opt-in.
  const page = await browser.newPage({ viewport: { width: 350, height: 800 } });
  await page.goto(`${url}?cancel=1`);
  await page.getByRole("button", { name: "导出与本地分享", exact: true }).click();
  const dialog = page.getByRole("dialog");
  const menuBounds = await dialog.boundingBox();
  assert.ok(menuBounds.x >= 0 && menuBounds.x + menuBounds.width <= 351, JSON.stringify(menuBounds));
  await dialog.screenshot({ path: join(output, "menu-350.png") });
  await page.evaluate(() => { document.documentElement.style.fontSize = "22px"; });
  const largeMenuBounds = await dialog.boundingBox();
  assert.ok(largeMenuBounds.x >= 0 && largeMenuBounds.x + largeMenuBounds.width <= 351
    && largeMenuBounds.y >= 0 && largeMenuBounds.y + largeMenuBounds.height <= 801, JSON.stringify(largeMenuBounds));
  assert.equal(await dialog.evaluate(el => el.scrollWidth > el.clientWidth + 1), false);
  await dialog.screenshot({ path: join(output, "menu-350-large-font.png") });
  await page.getByRole("button", { name: "导出 HTML", exact: true }).click();
  await page.waitForFunction(() => window.exportReview.saves.length === 1, null, { timeout: 60000 });
  await page.getByRole("status").waitFor({ state: "hidden" });
  assert.equal(await dialog.isVisible(), true);
  assert.deepEqual(await page.evaluate(() => window.exportReview.notices), []);
  await dialog.getByRole("checkbox").check();
  await page.evaluate(() => { window.exportReview.cancel = false; });
  await page.getByRole("button", { name: "导出 HTML", exact: true }).click();
  await dialog.waitFor({ state: "hidden" });
  assert.match(await page.evaluate(() => window.exportReview.saves[1].content), /默认隐藏的内部过程/);
  // Browser fallback emits an actual downloadable HTML, not a Markdown body.
  await page.goto(`${url}?browser=1`);
  await page.getByRole("button", { name: "导出与本地分享", exact: true }).click();
  const download = page.waitForEvent("download");
  await page.getByRole("button", { name: "导出 HTML", exact: true }).click();
  const downloaded = await download;
  const downloadedPath = join(output, "browser-download.html");
  await downloaded.saveAs(downloadedPath);
  assert.ok(/<table(?:\s|>)/.test(readFileSync(downloadedPath, "utf8")), "download contains rendered table markup");
  assert.match(downloaded.suggestedFilename(), /\.html$/);
  await page.close();
  writeFileSync(join(output, "result.json"), JSON.stringify({ layouts: evidence, nativeSaveCancelRetry: true, browserDownload: true }, null, 2));
  console.log(`HTML export passed: ${evidence.length} offline reading layouts, embedded math fonts and diagrams, native cancel/retry and browser download. Evidence: ${output}`);
} finally {
  await browser?.close();
  await server.close();
}
