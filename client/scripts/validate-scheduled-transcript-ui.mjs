import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createUiReviewServer, reviewChromium } from "./ui-review-runtime.mjs";

const output = mkdtempSync(join(tmpdir(), "echo-scheduled-transcript-"));
const server = await createUiReviewServer(0);
let browser;
const evidence = [];
try {
  browser = await (await reviewChromium()).launch({ headless: true,
    executablePath: process.env.CHROME_BIN || "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" });
  for (const theme of ["light", "dark"]) for (const platform of ["mac", "other"]) {
    const page = await browser.newPage({ viewport: { width: 1280, height: 900 } });
    const errors = [];
    page.on("pageerror", error => errors.push(error.message));
    await page.goto(`http://127.0.0.1:${server.httpServer.address().port}/__ui-review?surface=conversation&scheduledReplay=1&theme=${theme}&platform=${platform}`);
    await page.getByText("核实资料后生成本次定时任务报告。", { exact: true }).waitFor();
    await page.evaluate(() => window.scheduledTranscriptReview.progress());
    await page.getByRole("heading", { name: "本次唯一结论", exact: true }).waitFor();
    assert.equal(await page.locator(".execution-process").count(), 1);
    await page.screenshot({ path: join(output, `${theme}-${platform}-live.png`) });
    await page.evaluate(() => {
      window.scheduledTranscriptReview.complete();
      window.scheduledTranscriptReview.replay();
    });
    await page.waitForTimeout(250);
    assert.equal(await page.getByRole("heading", { name: "本次唯一结论", exact: true }).count(), 1, "six history deliveries must leave one conclusion");
    assert.equal(await page.locator(".execution-process").count(), 1, "six history deliveries must leave one process");
    const processHeader = page.locator(".execution-process__header");
    if (await processHeader.getAttribute("aria-expanded") !== "true") await processHeader.click();
    await page.locator('[data-tool-call-id="scheduled-tool"]').waitFor();
    assert.equal(await page.locator('[data-tool-call-id="scheduled-tool"]').count(), 1, "the process contains the operation once");
    assert.equal(await page.getByText("核实资料后生成本次定时任务报告。", { exact: true }).count(), 1, "canonical user echo must not be duplicated");
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth + 1), false);
    assert.deepEqual(errors, []);
    await page.screenshot({ path: join(output, `${theme}-${platform}-replayed-six-times.png`) });
    evidence.push({ theme, platform, replayDeliveries: 6, conclusions: 1, operations: 1, errors });
    await page.close();
  }
  writeFileSync(join(output, "evidence.json"), JSON.stringify(evidence, null, 2));
  console.log(JSON.stringify({ output, passed: evidence.length, evidence }, null, 2));
} finally {
  await browser?.close();
  await server.close();
}
