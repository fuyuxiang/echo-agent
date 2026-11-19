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
    const conversation = page.getByTestId("conversation");
    const indicator = conversation.locator(".msg__loading-status .waiting-indicator");
    await indicator.waitFor();
    await page.evaluate(async () => Promise.all([...document.images].map(image => image.decode())));
    const response = await page.request.get(new URL("/app-icon.png", page.url()).href);
    assert.ok((await response.body()).equals(readFileSync("app-icon.png")), "Browser did not load the current app artwork");
    const waitForVisibleMotion = () => page.waitForFunction(() => document.querySelector('[data-testid="conversation"] .waiting-indicator')?.dataset.motion === "on");
    await waitForVisibleMotion();
    assert.equal(await page.locator(".icon-review > header img").getAttribute("src"), "/app-icon.png");
    assert.equal(await conversation.locator("img").count(), 0, "Waiting uses a status dot in the transcript");
    assert.deepEqual(await indicator.evaluate(el => [el.clientWidth, el.clientHeight]), [6, 6]);
    assert.equal(await indicator.evaluate(el => getComputedStyle(el).animationPlayState), "running");
    assert.equal(await indicator.evaluate(el => getComputedStyle(el).animationName), "processStatusPulse");
    const trigger = page.getByRole("button", { name: "任务模型：工作模型", exact: true });
    assert.equal(await trigger.locator(".model-identity").count(), 1);
    assert.equal(await trigger.locator(".waiting-indicator").count(), 0);
    await trigger.click();
    const option = page.getByRole("option", { name: /工作模型/ });
    assert.equal(await option.locator(".model-identity").count(), 1);
    assert.equal(await option.locator(".waiting-indicator").count(), 0, "Model identity must stay still");
    await page.getByRole("dialog", { name: "任务模型", exact: true }).screenshot({ path: join(output, `model-menu-${theme}-${scale}x.png`) });
    await page.keyboard.press("Escape");
    await page.emulateMedia({ reducedMotion: "reduce" });
    assert.equal(await indicator.evaluate(el => getComputedStyle(el).animationName), "none");
    assert.equal(await indicator.evaluate(el => getComputedStyle(el).opacity), "1");
    await page.emulateMedia({ reducedMotion: "no-preference" });
    const offscreen = page.locator('[data-testid="offscreen"] .waiting-indicator');
    assert.equal(await offscreen.getAttribute("data-motion"), "off");
    await offscreen.scrollIntoViewIfNeeded();
    await page.waitForFunction(() => document.querySelector('[data-testid="offscreen"] .waiting-indicator')?.dataset.motion === "on"
      && document.querySelector('[data-testid="conversation"] .waiting-indicator')?.dataset.motion === "off");
    await page.getByTestId("scroll-area").evaluate(el => { el.scrollTop = 0; });
    await page.getByRole("button", { name: "完成回复", exact: true }).click();
    await indicator.waitFor({ state: "detached" });
    assert.equal(await conversation.locator(".msg__header, img").count(), 0);
    await page.getByRole("button", { name: "继续等待", exact: true }).click();
    await waitForVisibleMotion();
    for (const state of ["等待用户确认", "模拟错误"]) {
      await page.getByRole("button", { name: state, exact: true }).click();
      await indicator.waitFor({ state: "detached" });
      await page.getByRole("button", { name: "继续等待", exact: true }).click();
      await waitForVisibleMotion();
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
