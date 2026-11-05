import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createUiReviewServer, reviewChromium, reviewOutput } from "./ui-review-runtime.mjs";

const output = reviewOutput(mkdtempSync(join(tmpdir(), "echo-queue-admission-")));
const server = await createUiReviewServer(0);
const errors = [], evidence = [];
let browser;
try {
  browser = await (await reviewChromium()).launch({ headless: true, executablePath: process.env.CHROME_BIN || "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" });
  const page = await browser.newPage();
  page.on("pageerror", error => errors.push(error.message));
  const state = () => page.evaluate(() => window.queueReview.state());
  const panel = page.getByRole("list", { name: "待发送队列", exact: true });
  const open = async (theme, count = 1) => {
    await page.goto(`http://127.0.0.1:${server.httpServer.address().port}/__ui-review?surface=queue-admission&theme=${theme}&count=${count}`);
    await panel.waitFor();
    await panel.getByRole("button", { name: "中断当前回复并立即发送这条", exact: true }).first().click();
    await page.waitForFunction(() => window.queueReview.state().sent.length === 1);
    assert.equal((await state()).queue[0].status, "sending", "a local click is not native admission");
  };
  for (const [width, font] of [[1280, 16], [390, 22]]) for (const theme of ["light", "dark"]) {
    await page.setViewportSize({ width, height: 900 });
    await open(theme);
    await page.evaluate(font => { document.documentElement.style.fontSize = `${font}px`; }, font);
    await page.screenshot({ path: join(output, `before-${width}-${theme}.png`) });
    await page.evaluate(() => window.queueReview.replay());
    assert.equal((await state()).queue[0].status, "sending", "historical echoes do not acknowledge sends");
    await page.evaluate(() => window.queueReview.acknowledge());
    await panel.waitFor({ state: "hidden" });
    let current = await state();
    assert(current.streaming && current.queue[0].status === "accepted", "execution continues after the pending panel disappears");
    assert.equal(current.sent.length, 1);
    assert(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1), "no horizontal overflow");
    await page.screenshot({ path: join(output, `accepted-${width}-${theme}.png`) });
    await page.evaluate(() => window.queueReview.complete());
    assert.equal((await state()).queue.length, 0);

    await open(theme, 2);
    await page.evaluate(() => window.queueReview.acknowledge(true));
    await panel.getByText("待发送队列(1)", { exact: true }).waitFor();
    assert.equal((await state()).sent.length, 1, "admission never starts the next message");
    assert.equal(await panel.getByRole("button", { name: "编辑消息：1123", exact: true }).count(), 0);
    await page.evaluate(() => window.queueReview.complete());
    assert.equal((await state()).sent.length, 2, "normal completion dispatches exactly one next message");
    await page.evaluate(() => window.queueReview.acknowledge());
    await panel.waitFor({ state: "hidden" });
    await page.evaluate(() => window.queueReview.complete("error"));
    await panel.getByText("执行失败，等待确认", { exact: true }).waitFor();
    assert.equal((await state()).sent.length, 2, "failed execution does not replay itself");
    await panel.getByRole("button", { name: "恢复", exact: true }).click();
    const dialog = page.getByRole("dialog", { name: "确认再次发送这条消息？", exact: true });
    await dialog.waitFor();
    await page.screenshot({ path: join(output, `failed-review-${width}-${theme}.png`) });
    await dialog.getByRole("button", { name: "取消", exact: true }).click();
    assert.equal((await state()).queue[0].status, "paused");

    await open(theme);
    await page.evaluate(() => window.queueReview.reject());
    await panel.getByRole("button", { name: "恢复", exact: true }).waitFor();
    current = await state();
    assert.equal(current.queue[0].text, "1123");
    assert.deepEqual(current.queue[0].attachments, ["/review/需求.docx"]);
    assert.equal(current.queue[0].status, "paused");
    evidence.push({ width, font, theme, scenarios: ["native-admission", "replay", "legacy-echo", "sequential-send", "failure-review", "rejected-send"] });
  }
  assert.deepEqual(errors, []);
  writeFileSync(join(output, "results.json"), JSON.stringify({ evidence, errors }, null, 2));
  console.log(JSON.stringify({ passed: true, layouts: evidence.length, scenarios: evidence.length * 6, output, errors }));
} finally {
  await browser?.close();
  await server.close();
}
