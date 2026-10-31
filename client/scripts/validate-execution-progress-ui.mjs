import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createUiReviewServer, reviewChromium, reviewOutput } from "./ui-review-runtime.mjs";

const output = reviewOutput(mkdtempSync(join(tmpdir(), "echo-execution-progress-")));
const server = await createUiReviewServer(0, { freezeMotion: false });
let browser, page;
const errors = [], evidence = [];
try {
  browser = await (await reviewChromium()).launch({ headless: true, ...(process.env.CHROME_BIN
    ? { executablePath: process.env.CHROME_BIN } : process.platform === "darwin"
      ? { executablePath: "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" } : {}) });
  const stage = value => page.evaluate(async value => {
    const { useSessionStore } = await import("/src/stores/session-store.ts");
    const store = useSessionStore.getState();
    const tool = (id, status) => ({ kind: "tool_call", streamId: "stage-1", stageTitle: "核对资料与准备演示结构",
      toolCall: { toolCallId: id, kind: "read_file", title: `检查资料 · ${id}`, status,
        content: [{ type: "text", text: "隔离工具输出，验证详情可打开。" }] } });
    const parts = [
      { kind: "text", phase: "commentary", streamId: "stage-1", stageTitle: "核对资料与准备演示结构",
        text: "资料来源已经确认。正在核对数据和演示结构。" },
      { kind: "thought", streamId: "stage-1", stageTitle: "核对资料与准备演示结构",
        text: value === "long-record" ? Array.from({ length: 60 }, (_, i) => `分析第 ${i + 1} 段：检查资料来源、数据和交互反馈，保留完整过程以供回看。`).join("\n\n") : "分析片段。".repeat(200) + "分析原文末尾" },
      tool("ongoing", ["tools", "waiting"].includes(value) ? "in_progress" : "completed"),
      ...["old", "recent", "latest"].map(id => tool(id, "completed")),
    ];
    if (["answer", "long-answer", "complete"].includes(value)) parts.push({ kind: "text", phase: "final_answer",
      streamId: "answer-1", text: value === "long-answer" ? Array.from({ length: 30 }, (_, i) => `第 ${i + 1} 段：整理资料并说明数据来源与适用条件。`).join("\n\n") : "资料已整理，以下是最终结果。" });
    store.setMessages([
      { id: "progress-user", role: "user", complete: true, parts: [{ kind: "text", text: "整理资料并制作演示文稿。" }] },
      { id: store.streamingMessageId, role: "assistant", complete: value === "complete", startedAt: Date.now() - 76_000,
        lastActivityAt: Date.now() - (value === "waiting" ? 45_000 : 0), completedAt: value === "complete" ? Date.now() : undefined,
        stopReason: value === "complete" ? "end_turn" : undefined, parts },
    ]);
    if (value === "complete") store.markComplete({ sessionId: store.sessionId, promptId: "", stopReason: "end_turn" });
  }, value);

  for (const [width, height, theme, font] of [[1440, 1000, "light", 13], [1440, 1000, "dark", 13], [1024, 680, "light", 13], [1024, 680, "dark", 18]]) {
    await page?.close();
    page = await browser.newPage({ reducedMotion: "reduce" });
    page.setDefaultTimeout(15000); page.setDefaultNavigationTimeout(60000);
    page.on("pageerror", error => errors.push(error.message));
    await page.setViewportSize({ width, height });
    await page.goto(`http://127.0.0.1:${server.httpServer.address().port}/__ui-review?surface=execution&state=running&theme=${theme}`);
    await page.locator(".execution-process__header").waitFor();
    await page.evaluate(font => document.documentElement.style.fontSize = `${16 * font / 13}px`, font);
    await stage("tools");
    const process = page.locator(".execution-process"), header = process.locator(".execution-process__header");
    await process.getByText("核对资料与准备演示结构", { exact: true }).waitFor();
    assert.equal(await header.getAttribute("aria-expanded"), "false");
    assert.match(await header.textContent(), /展开过程/);
    assert.equal(await process.getByRole("button").count(), 1, "one entry for records");
    assert.equal(await process.locator(".toolcall, .execution-process__reasoning").count(), 0);
    assert.match(await process.locator(".execution-process__excerpt").textContent(), /资料来源已经确认。正在核对数据/);
    assert.match(await process.locator(".execution-process__metrics").textContent(), /已完成 3 项操作 · 1 项进行中/);
    const geometry = await process.evaluate(element => {
      const box = element.getBoundingClientRect(), excerpt = element.querySelector(".execution-process__excerpt p");
      return { x: box.x, right: box.right, clipped: element.scrollWidth > element.clientWidth + 1,
        clientWidth: element.clientWidth, scrollWidth: element.scrollWidth,
        overflowing: Array.from(element.querySelectorAll("*")).filter(node => node.getBoundingClientRect().right > box.right + 1).map(node => ({ class: node.className, right: node.getBoundingClientRect().right })),
        overflow: document.documentElement.scrollWidth > innerWidth + 1, excerptWhiteSpace: getComputedStyle(excerpt).whiteSpace,
        statusAnimation: getComputedStyle(element.querySelector(".execution-process__status")).animationName,
        chevronTransition: getComputedStyle(element.querySelector(".execution-process__chevron")).transitionDuration };
    });
    assert(!geometry.clipped && !geometry.overflow && geometry.x >= 0 && geometry.right <= width + 1, JSON.stringify({ width, theme, font, geometry }));
    assert.equal(geometry.excerptWhiteSpace, "normal");
    assert.equal(geometry.statusAnimation, "none");
    assert(geometry.chevronTransition.split(",").every(duration => parseFloat(duration) <= 0.00001), "reduced motion disables the chevron transition");
    await page.screenshot({ path: join(output, `progress-${width}-${theme}-${font}.png`) });

    await header.focus(); await page.keyboard.press("Enter");
    assert.match(await header.textContent(), /收起过程/);
    assert.equal(await process.locator(".toolcall").count(), 4, "one click reveals all operations");
    const body = process.locator(".execution-process__body");
    assert.equal(await body.evaluate(el => getComputedStyle(el).overflowY), "visible", "transcript owns scrolling");
    const focused = process.locator('[data-tool-call-id="ongoing"]');
    await focused.focus();
    await stage("thinking");
    await focused.getByText("完成", { exact: true }).waitFor();
    assert(await focused.evaluate(el => el === document.activeElement), "tool updates preserve focus");
    await focused.click();
    await page.getByText("隔离工具输出，验证详情可打开。", { exact: true }).waitFor();
    await page.getByRole("button", { name: "关闭面板", exact: true }).click();
    await process.getByText("分析详情", { exact: true }).click();
    await process.getByRole("button", { name: "展开全文", exact: true }).click();
    await process.getByText(/分析原文末尾/).waitFor();
    await process.getByRole("button", { name: "收起全文", exact: true }).click();
    assert.equal(await process.getByText(/分析原文末尾/).count(), 0);
    await page.screenshot({ path: join(output, `records-${width}-${theme}-${font}.png`) });
    await stage("long-record");
    await process.getByRole("button", { name: "展开全文", exact: true }).click();
    const collapse = process.getByRole("button", { name: "收起过程", exact: true });
    await collapse.scrollIntoViewIfNeeded();
    assert(await body.evaluate(el => el.getBoundingClientRect().height > document.querySelector(".chatview__scroll").clientHeight), "long records exceed the viewport");
    await collapse.focus(); await page.keyboard.press("Enter");
    await page.waitForFunction(() => {
      const entry = document.querySelector(".execution-process__header");
      const viewport = document.querySelector(".chatview__scroll").getBoundingClientRect();
      const box = entry.getBoundingClientRect();
      return !document.querySelector(".execution-process__body") && entry === document.activeElement
        && box.top >= viewport.top - 1 && box.bottom <= viewport.bottom + 1;
    });
    assert.match(await header.textContent(), /展开过程/);

    await stage("waiting");
    await process.getByRole("note").waitFor();
    assert.match(await process.getByRole("note").textContent(), /未收到新进展.*等待操作返回/);
    await stage("tools");
    await process.getByRole("note").waitFor({ state: "hidden" });
    await stage("answer");
    await page.locator(".msg__writing").waitFor();
    assert.equal(await process.locator(".execution-process__excerpt").count(), 0);
    assert.equal(await page.locator(".msg--assistant [role=status]").count(), 1);
    await stage("long-answer");
    // Inspecting and collapsing a long record is deliberate reading intent.
    // Resume following explicitly before asserting that new output stays visible.
    const latest = page.getByRole("button", { name: "回到最新消息并恢复自动跟随", exact: true });
    if (await latest.isVisible()) await latest.click();
    await page.waitForFunction(() => {
      const status = document.querySelector(".msg__writing")?.getBoundingClientRect();
      const viewport = document.querySelector(".chatview__scroll")?.getBoundingClientRect();
      return status && viewport && status.top >= viewport.top && status.bottom <= viewport.bottom + 1;
    });
    await stage("complete");
    await process.getByText("本轮已完成", { exact: true }).waitFor();
    assert.equal(await page.locator(".execution-process--running, .msg__writing").count(), 0);
    assert.equal(await header.getAttribute("aria-expanded"), "false");
    assert.match(await header.textContent(), /查看过程/);
    evidence.push({ width, height, theme, font, geometry });
  }
  assert.deepEqual(errors, []);
  writeFileSync(join(output, "evidence.json"), JSON.stringify({ layouts: evidence, errors }, null, 2));
  console.log(JSON.stringify({ ok: true, output, layouts: evidence.length,
    checks: ["task progress", "context", "one record entry", "all operations", "details", "focus", "long analysis", "footer collapse returns to overview", "actual waiting", "answer feedback", "completion", "reduced motion"], errors }, null, 2));
} catch (error) {
  if (page) await page.screenshot({ path: join(output, "failure.png") }).catch(() => {});
  throw error;
} finally { await browser?.close(); await server.close(); }
