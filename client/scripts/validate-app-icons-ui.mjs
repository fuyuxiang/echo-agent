import assert from "node:assert/strict";
import { existsSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createUiReviewServer, reviewChromium, reviewOutput } from "./ui-review-runtime.mjs";

const output = reviewOutput(mkdtempSync(join(tmpdir(), "echo-app-icons-review-")));
const server = await createUiReviewServer(0, { fixture: "/scripts/fixtures/app-icons-review.tsx", freezeMotion: false });
const evidence = [];
let browser;
try {
  const chrome = process.env.CHROME_BIN || (process.platform === "darwin"
    ? "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" : undefined);
  browser = await (await reviewChromium()).launch({ headless: true,
    ...(chrome && existsSync(chrome) ? { executablePath: chrome } : {}) });
  for (const theme of ["light", "dark"]) for (const scale of [1, 2]) {
    const page = await browser.newPage({ viewport: { width: 900, height: 1100 }, deviceScaleFactor: scale });
    page.setDefaultTimeout(15000);
    const errors = [];
    page.on("pageerror", error => errors.push(error.message));
    page.on("requestfailed", request => errors.push(`${request.failure()?.errorText}: ${request.url()}`));
    page.on("response", response => { if (response.status() >= 400) errors.push(`${response.status()}: ${response.url()}`); });
    await page.goto(`http://127.0.0.1:${server.httpServer.address().port}/__ui-review?theme=${theme}`);
    const mark = page.locator('[data-testid="conversation"] .echo-mark');
    await mark.waitFor();
    await page.evaluate(async () => Promise.all([...document.images].map(image => image.decode())));
    const response = await page.request.get(new URL("/app-icon.png", page.url()).href);
    assert.ok((await response.body()).equals(readFileSync("app-icon.png")), "Browser did not load the current app artwork");
    await page.waitForFunction(() => document.querySelector('[data-testid="conversation"] .echo-mark')?.dataset.motion === "on");
    assert.equal(await mark.locator("img").getAttribute("src"), "/app-icon.png");
    assert.deepEqual(await mark.evaluate(el => [el.clientWidth, el.clientHeight]), [24, 24]);
    assert.equal(await mark.evaluate(el => getComputedStyle(el, "::after").animationPlayState), "running");
    assert.equal(await mark.evaluate(el => getComputedStyle(el, "::after").animationName), "echo-mark-breathe");
    assert.equal(await mark.locator("img").evaluate(el => getComputedStyle(el).transform), "none", "Artwork itself should stay still");
    const trigger = page.getByRole("button", { name: "任务模型：Echo Reed", exact: true });
    assert.deepEqual(await trigger.locator(".echo-mark img").evaluate(el => [el.clientWidth, el.clientHeight]), [18, 18]);
    await trigger.click();
    const option = page.getByRole("option", { name: /Echo Reed/ });
    assert.deepEqual(await option.locator(".echo-mark img").evaluate(el => [el.clientWidth, el.clientHeight]), [22, 22]);
    assert.equal(await option.locator(".echo-mark--active").count(), 0, "Model identity must stay still");
    await page.getByRole("dialog", { name: "任务模型", exact: true }).screenshot({ path: join(output, `model-menu-${theme}-${scale}x.png`) });
    await page.keyboard.press("Escape");
    await page.emulateMedia({ reducedMotion: "reduce" });
    assert.equal(await mark.evaluate(el => getComputedStyle(el, "::after").animationName), "none");
    assert.equal(await mark.evaluate(el => getComputedStyle(el, "::after").opacity), "0");
    await page.emulateMedia({ reducedMotion: "no-preference" });
    const offscreen = page.locator('[data-testid="offscreen"] .echo-mark');
    assert.equal(await offscreen.getAttribute("data-motion"), "off");
    await offscreen.scrollIntoViewIfNeeded();
    await page.waitForFunction(() => document.querySelector('[data-testid="offscreen"] .echo-mark')?.dataset.motion === "on"
      && document.querySelector('[data-testid="conversation"] .echo-mark')?.dataset.motion === "off");
    await page.getByTestId("scroll-area").evaluate(el => { el.scrollTop = 0; });
    await page.getByRole("button", { name: "完成回复", exact: true }).click();
    assert.equal(await mark.getAttribute("data-motion"), "off");
    assert.equal(await mark.evaluate(el => getComputedStyle(el, "::after").animationName), "none");
    assert.equal(await mark.evaluate(el => getComputedStyle(el, "::after").opacity), "0");
    await page.getByRole("button", { name: "继续等待", exact: true }).click();
    await page.waitForFunction(() => document.querySelector('[data-testid="conversation"] .echo-mark')?.dataset.motion === "on");
    for (const state of ["等待用户确认", "模拟错误"]) {
      await page.getByRole("button", { name: state, exact: true }).click();
      assert.equal(await mark.getAttribute("data-motion"), "off");
      assert.equal(await mark.evaluate(el => getComputedStyle(el, "::after").animationName), "none");
      await page.getByRole("button", { name: "继续等待", exact: true }).click();
      await page.waitForFunction(() => document.querySelector('[data-testid="conversation"] .echo-mark')?.dataset.motion === "on");
    }
    await page.screenshot({ path: join(output, `icons-${theme}-${scale}x.png`), animations: "disabled" });
    assert.deepEqual(errors, []);
    evidence.push({ theme, scale, decodedImages: await page.locator("img").count(), passed: true });
    await page.close();
  }
  console.log(`App icons passed: ${evidence.length} light/dark and DPI layouts, exact artwork, model identity, completion/decision/error states, offscreen pause and reduced motion. Screenshots: ${output}`);
} finally {
  try { await browser?.close(); }
  finally {
    try { await server.close(); }
    finally { writeFileSync(join(output, "results.json"), JSON.stringify(evidence, null, 2) + "\n"); }
  }
}
