import assert from "node:assert/strict";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createUiReviewServer, reviewChromium } from "./ui-review-runtime.mjs";

const output = mkdtempSync(join(tmpdir(), "echo-predictions-ui-"));
const server = await createUiReviewServer(0, { fixture: "/scripts/fixtures/prompt-prediction-review.tsx" });
const base = `http://127.0.0.1:${server.httpServer.address().port}`;
let browser;
try {
  browser = await (await reviewChromium()).launch({ headless: true,
    executablePath: process.env.CHROME_BIN || "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" });
  for (const [width, height, theme] of [[1280, 800, "light"], [1024, 680, "dark"], [640, 720, "light"]]) {
    const page = await browser.newPage({ viewport: { width, height } });
    page.setDefaultTimeout(45000);
    page.setDefaultNavigationTimeout(60000);
    const errors = [];
    page.on("pageerror", error => errors.push(error.message));
    await page.goto(`${base}/__ui-review?surface=conversation&theme=${theme}`, { waitUntil: "networkidle" });
    const input = page.locator(".echo-composer__input");
    const ghost = page.getByTestId("composer-prediction");
    await input.waitFor();
    const turn = async () => {
      const before = await page.evaluate(() => window.predictionReview.requests.length);
      await page.evaluate(() => window.predictionReview.start());
      await page.evaluate(() => window.predictionReview.complete());
      await page.waitForFunction(count => window.predictionReview.requests.length === count + 1, before);
      return before;
    };
    const text = "按这个方案完成中文适配、输入建议和草稿保护，完成后运行测试验证。";
    const first = await turn();
    await page.evaluate(({ index, text }) => window.predictionReview.answer(index, text), { index: first, text });
    await ghost.waitFor();
    assert.equal(await input.inputValue(), "");
    assert.equal(await page.evaluate(() => window.predictionReview.draft()), "");
    await input.press("Enter");
    assert.equal(await input.inputValue(), "");
    const geometry = await ghost.evaluate(element => {
      const r = element.getBoundingClientRect();
      const footer = element.closest(".echo-composer").querySelector(".echo-composer__footer").getBoundingClientRect();
      return { x: r.x, right: r.right, bottom: r.bottom, footer: footer.top, viewport: innerWidth, overflow: document.documentElement.scrollWidth > innerWidth };
    });
    assert(!geometry.overflow && geometry.x >= 0 && geometry.right <= geometry.viewport + 1 && geometry.bottom <= geometry.footer + 1,
      `prediction overlaps controls: ${JSON.stringify(geometry)}`);
    await page.screenshot({ path: join(output, `prediction-${width}-${theme}.png`) });
    await input.press("Tab");
    assert.equal(await input.inputValue(), text);
    assert.equal(await page.evaluate(() => window.predictionReview.draft()), text);
    assert.equal(await ghost.count(), 0);
    await page.evaluate(() => window.predictionReview.switchSession("review-other"));
    assert.equal(await input.inputValue(), "");
    await page.evaluate(() => window.predictionReview.switchSession("review-conversation"));
    await page.waitForFunction(text => document.querySelector(".echo-composer__input").value === text, text);
    await input.fill("");

    const second = await turn();
    await input.fill("用户自己的下一步");
    await page.evaluate(index => window.predictionReview.answer(index, "这条迟到建议必须忽略"), second);
    assert.equal(await input.inputValue(), "用户自己的下一步");
    assert.equal(await ghost.count(), 0);
    await input.fill("");

    const third = await turn();
    await page.evaluate(() => window.predictionReview.start());
    await page.evaluate(index => window.predictionReview.answer(index, "旧轮次建议"), third);
    assert.equal(await ghost.count(), 0);
    await page.evaluate(() => window.predictionReview.complete());
    await page.waitForFunction(count => window.predictionReview.requests.length === count + 2, third);
    await page.evaluate(() => window.predictionReview.enabled(false));
    await page.evaluate(index => window.predictionReview.answer(index, "关闭后也不能出现"), third + 1);
    assert.equal(await ghost.count(), 0);
    assert.equal(await input.inputValue(), "");
    assert.deepEqual(errors, []);
    await page.close();
  }
  console.log(`Prediction UI verified in 3 viewport/theme combinations. Screenshots: ${output}`);
} finally {
  await browser?.close();
  await server.close();
}
