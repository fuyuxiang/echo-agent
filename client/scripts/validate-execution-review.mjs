import assert from "node:assert/strict";
import { writeFileSync } from "node:fs";
import { join } from "node:path";

/** Real production interaction with isolated ACP/lifecycle events. */
export async function validateExecutionReview(page, output) {
  const base = new URL(page.url()).origin;
  await page.setViewportSize({ width: 1280, height: 800 });
  await page.goto(`${base}/__ui-review?surface=execution&state=running&theme=light`);
  await page.locator('[data-execution-ready="true"]').waitFor();
  const process = page.locator(".execution-process__header").first();
  await process.waitFor();
  assert.equal(await process.getAttribute("aria-expanded"), "true", "interim commentary hides active work");
  assert.ok(await page.locator(".execution-process--running").count(), "interim commentary incorrectly completes active work");
  assert.equal(await page.locator(".toolcall--run").count(), 3, "unfinished tools are missing from the live process");
  await process.click();
  assert.equal(await process.getAttribute("aria-expanded"), "false");
  await process.click();
  await page.locator(".execution-process__body").waitFor();
  await page.locator(".toolcall").filter({ hasText: "pnpm test" }).first().click();
  await page.locator(".tool-side-panel").waitFor();
  assert.ok(await page.getByText("[isolated 150]", { exact: false }).count(), "long command output is missing");
  assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1), "tool details expand the page");
  await page.screenshot({ path: join(output, "execution-tool-details-1280-light.png") });

  await page.goto(`${base}/__ui-review?surface=execution&state=running&theme=dark`);
  await page.locator('[data-execution-ready="true"]').waitFor();
  const scroll = page.locator(".chatview__scroll");
  // Add enough real chunks to require scrolling, then use an actual wheel
  // gesture to express reading intent before subsequent content grows.
  for (let i = 0; i < 5; i++) await page.getByRole("button", { name: "追加流式内容", exact: true }).click();
  await scroll.hover();
  await page.mouse.wheel(0, -1500);
  await page.waitForTimeout(200);
  const reading = await scroll.evaluate(el => ({ top: el.scrollTop, height: el.scrollHeight, max: el.scrollHeight - el.clientHeight }));
  const before = reading.top;
  assert.ok(reading.max > 40 && before < reading.max - 40, "the reader is not actually above the bottom of overflowing content");
  await page.getByRole("button", { name: "追加流式内容", exact: true }).click();
  await page.waitForTimeout(200);
  const after = await scroll.evaluate(el => el.scrollTop);
  const grownHeight = await scroll.evaluate(el => el.scrollHeight);
  assert.ok(grownHeight > reading.height, "the streaming fixture did not grow rendered content");
  assert.ok(Math.abs(after - before) < 4, "stream growth pulls the reader to the bottom");
  await page.screenshot({ path: join(output, "execution-reading-position-1280-dark.png") });
  await page.getByRole("button", { name: "完成当前任务", exact: true }).click();
  await page.waitForFunction(() => !document.querySelector(".execution-process--running") && !document.querySelector(".toolcall--run"));
  await page.locator(".chatview__scroll").evaluate(el => { el.scrollTop = el.scrollHeight; });
  await page.getByRole("heading", { name: "隔离验证：界面评审交付", exact: true }).waitFor({ state: "attached" });
  assert.ok(await page.getByRole("button", { name: "追加流式内容", exact: true }).isDisabled(), "completed task still permits streaming");
  await page.screenshot({ path: join(output, "execution-completed-1280-dark.png") });

  await page.goto(`${base}/__ui-review?surface=runtime&state=running&theme=dark`);
  await page.locator('[data-execution-ready="true"]').waitFor();
  await page.getByRole("region", { name: "子代理运行时" }).waitFor();
  await page.getByRole("region", { name: "团队状态" }).waitFor();
  await page.getByText("review-isolated-team", { exact: true }).waitFor();
  const rows = page.locator(".subagent-panel__row");
  assert.equal(await rows.count(), 2, "parallel child agents are missing");
  await rows.first().click();
  assert.equal(await rows.first().getAttribute("aria-expanded"), "true");
  await page.screenshot({ path: join(output, "execution-runtime-expanded-1280-dark.png") });
  await page.getByRole("button", { name: "模拟工具失败", exact: true }).click();
  await rows.nth(1).click();
  assert.equal(await rows.nth(1).getAttribute("aria-expanded"), "true");
  await page.getByText("隔离失败：外部预览服务未配置，未发出真实请求。", { exact: true }).first().waitFor({ state: "attached" });
  await page.screenshot({ path: join(output, "execution-runtime-partial-failure-1280-dark.png") });
  await page.goto(`${base}/__ui-review?surface=runtime&state=running&teamState=error&theme=dark`);
  await page.getByRole("region", { name: "团队状态" }).getByRole("alert").waitFor();
  await page.screenshot({ path: join(output, "execution-team-error-1280-dark.png") });
  await page.goto(`${base}/__ui-review?surface=execution&state=running&theme=light`);
  await page.locator('[data-execution-ready="true"]').waitFor();
  await page.getByRole("button", { name: "停止生成", exact: true }).click();
  await page.getByText("已停止（发送新消息可继续此任务）", { exact: true }).waitFor();
  assert.ok(await page.getByRole("button", { name: "追加流式内容", exact: true }).isDisabled(), "stop ACK leaves the task streaming");
  assert.equal(await page.locator(".execution-process--running").count(), 0, "stop ACK leaves a running process header");
  await page.screenshot({ path: join(output, "execution-stopped-1280-light.png") });
  const evidence = { passed: true, screenshots: 7, checks: ["interim commentary keeps active process running/open", "process collapse/expand", "long tool output", "stream growth preserves reading position", "completion clears active state", "parallel runtime display", "partial failure details", "team read error", "stop ACK clears streaming state"], scroll: { before, after, heightBefore: reading.height, heightAfter: grownHeight, maxBefore: reading.max } };
  writeFileSync(join(output, "execution-review.json"), JSON.stringify(evidence, null, 2));
  return evidence;
}
