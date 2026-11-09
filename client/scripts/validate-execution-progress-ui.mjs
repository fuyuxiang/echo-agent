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
    if (["more-tools", "review-answer"].includes(value)) {
      parts.push(tool("newer", "completed"), tool("newest", "completed"));
    }
    if (["answer", "review-answer", "long-answer", "complete"].includes(value)) parts.push({ kind: "text", phase: "final_answer",
      streamId: "answer-1", text: value === "long-answer" ? Array.from({ length: 30 }, (_, i) => `第 ${i + 1} 段：整理资料并说明数据来源与适用条件。`).join("\n\n") : "资料已整理，以下是最终结果。" });
    store.setMessages([
      { id: "progress-user", role: "user", complete: true, parts: [{ kind: "text", text: "整理资料并制作演示文稿。" }] },
      { id: store.streamingMessageId, role: "assistant", complete: value === "complete", startedAt: Date.now() - 76_000,
        lastActivityAt: Date.now() - (value === "waiting" ? 45_000 : 0), completedAt: value === "complete" ? Date.now() : undefined,
        stopReason: value === "complete" ? "end_turn" : undefined, parts },
    ]);
    if (value === "complete") store.markComplete({ sessionId: store.sessionId, promptId: "", stopReason: "end_turn" });
  }, value);
  const pending = (kind, open) => page.evaluate(async ({ kind, open }) => {
    const { useSessionStore } = await import("/src/stores/session-store.ts");
    const { sessionId } = useSessionStore.getState();
    const requestId = `execution-progress-${kind}`;
    if (kind === "question") {
      const { useQuestionStore } = await import("/src/stores/question-store.ts");
      const questions = useQuestionStore.getState();
      if (open) questions.request({ requestId, sessionId, toolCallId: "ongoing", title: "确认资料范围", mode: "default",
        questions: [{ id: "scope", question: "需要核对哪些资料？", options: [], multiSelect: false }] });
      else questions.dismiss(requestId, sessionId);
    } else {
      const { usePermissionStore } = await import("/src/stores/permission-store.ts");
      const permissions = usePermissionStore.getState();
      if (open) permissions.request({ requestId, sessionId, toolCallId: "ongoing", toolKind: "read_file", title: "读取资料",
        options: [{ optionId: "allow", title: "允许一次", kind: "allow_once" }] });
      else permissions.dismiss(requestId, sessionId);
    }
  }, { kind, open });

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
    assert.equal(await header.getAttribute("aria-expanded"), "true");
    assert.match(await header.textContent(), /收起过程/);
    assert.equal(await process.locator(".toolcall").count(), 4, "running records are immediately available");
    assert.match(await process.locator(".execution-process__progress").textContent(), /资料来源已经确认。正在核对数据/);
    assert.match(await process.locator(".execution-process__metrics").textContent(), /已完成 3 项操作 · 1 项进行中/);
    const geometry = await process.evaluate(element => {
      const box = element.getBoundingClientRect(), progress = element.querySelector(".execution-process__progress p");
      return { x: box.x, right: box.right, clipped: element.scrollWidth > element.clientWidth + 1,
        clientWidth: element.clientWidth, scrollWidth: element.scrollWidth,
        overflowing: Array.from(element.querySelectorAll("*")).filter(node => node.getBoundingClientRect().right > box.right + 1).map(node => ({ class: node.className, right: node.getBoundingClientRect().right })),
        overflow: document.documentElement.scrollWidth > innerWidth + 1, progressWhiteSpace: getComputedStyle(progress).whiteSpace,
        statusAnimation: getComputedStyle(element.querySelector(".execution-process__status")).animationName,
        chevronTransition: getComputedStyle(element.querySelector(".execution-process__chevron")).transitionDuration };
    });
    assert(!geometry.clipped && !geometry.overflow && geometry.x >= 0 && geometry.right <= width + 1, JSON.stringify({ width, theme, font, geometry }));
    assert.equal(geometry.progressWhiteSpace, "normal");
    assert.equal(geometry.statusAnimation, "none");
    assert(geometry.chevronTransition.split(",").every(duration => parseFloat(duration) <= 0.00001), "reduced motion disables the chevron transition");
    await page.screenshot({ path: join(output, `progress-${width}-${theme}-${font}.png`) });

    const body = process.locator(".execution-process__body");
    const latestProcess = process.getByRole("button", { name: "回到最新进展", exact: true });
    const latestMessage = page.getByRole("button", { name: "回到最新消息并恢复自动跟随", exact: true });
    const atLatest = () => body.evaluate(el => el.scrollHeight - el.scrollTop - el.clientHeight <= 2);
    assert(await atLatest(), "running records follow the latest progress");
    assert(await body.evaluate(el => {
      const style = getComputedStyle(el);
      const box = el.getBoundingClientRect();
      const message = el.closest(".msg__body").getBoundingClientRect();
      return style.overflowY === "auto" && box.height <= parseFloat(style.lineHeight) * 7 + 1
        && box.width === message.width && box.left === message.left;
    }), "the seven-line viewport matches the answer width");

    await body.focus();
    const latestTop = await body.evaluate(el => el.scrollTop);
    await page.keyboard.press("ArrowUp");
    await latestProcess.waitFor();
    const keyboardTop = await body.evaluate(el => el.scrollTop);
    assert(keyboardTop < latestTop, "ArrowUp scrolls the records");
    await page.keyboard.press("ArrowDown");
    assert(await atLatest(), "ArrowDown can resume following at the bottom");
    await latestProcess.waitFor({ state: "detached" });
    await body.hover();
    await page.mouse.wheel(0, -70);
    await latestProcess.waitFor();
    await page.waitForFunction(top => document.querySelector(".execution-process__body").scrollTop < top - 2, latestTop);
    await latestMessage.waitFor();
    assert.match(await latestMessage.textContent(), /回到最新消息/);
    // Activate the record control without Playwright scrolling the containing transcript.
    await latestProcess.evaluate(button => button.click());
    assert(await atLatest());
    assert(await latestMessage.isVisible(), "resuming records keeps the transcript action available");
    await page.keyboard.press("ArrowUp");
    await latestProcess.waitFor();
    const readingTop = await body.evaluate(el => el.scrollTop);
    const originalSession = await page.evaluate(async () => {
      const { useSessionStore } = await import("/src/stores/session-store.ts");
      const store = useSessionStore.getState();
      const sessionId = store.sessionId;
      store.setSession("execution-progress-other");
      return sessionId;
    });
    await body.waitFor({ state: "detached" });
    await page.evaluate(async id => {
      const { useSessionStore } = await import("/src/stores/session-store.ts");
      useSessionStore.getState().setSession(id);
    }, originalSession);
    await latestProcess.waitFor();
    assert.equal(await body.evaluate(el => el.scrollTop), readingTop, "switching sessions restores the record position");
    await stage("more-tools");
    await process.locator('[data-tool-call-id="newest"]').waitFor({ state: "attached" });
    assert.equal(await body.evaluate(el => el.scrollTop), readingTop, "new records preserve the reader's position");
    await stage("review-answer");
    await page.locator(".msg__writing").waitFor();
    assert.equal(await header.getAttribute("aria-expanded"), "true", "an answer leaves records open while reviewing");
    assert.equal(await body.evaluate(el => el.scrollTop), readingTop);
    await latestProcess.click();
    assert(await atLatest(), "one action returns to the latest records");
    assert(await body.evaluate(el => el === document.activeElement));
    await page.evaluate(() => document.querySelector(".chatview__jump-latest")?.click());
    await latestMessage.waitFor({ state: "detached" });

    await stage("waiting");
    await process.getByRole("note").waitFor();
    assert.match(await process.getByRole("note").textContent(), /未收到新进展.*等待操作返回/);
    await stage("tools");
    await process.getByRole("note").waitFor({ state: "hidden" });
    await stage("answer");
    await body.waitFor({ state: "detached" });
    for (const [kind, reason] of [["question", "等待你补充信息"], ["permission", "等待你批准操作"]]) {
      await pending(kind, true);
      await process.getByText(reason, { exact: true }).waitFor();
      assert.equal(await header.getAttribute("aria-expanded"), "true", reason);
      await pending(kind, false);
      await body.waitFor({ state: "detached" });
    }
    await stage("tools");
    await body.waitFor();
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
    assert(await body.evaluate(el => el.scrollHeight > el.clientHeight * 3), "long records remain scrollable inside the capped viewport");
    await collapse.focus(); await page.keyboard.press("Enter");
    await page.waitForFunction(() => {
      const entry = document.querySelector(".execution-process__header");
      const viewport = document.querySelector(".chatview__scroll").getBoundingClientRect();
      const box = entry.getBoundingClientRect();
      return !document.querySelector(".execution-process__body") && entry === document.activeElement
        && box.top >= viewport.top - 1 && box.bottom <= viewport.bottom + 1;
    });
    assert.match(await header.textContent(), /展开过程/);

    await stage("answer");
    await page.locator(".msg__writing").waitFor();
    assert.equal(await process.locator(".execution-process__excerpt").count(), 0);
    assert.equal(await page.locator(".msg--assistant [role=status]").count(), 1);
    await stage("long-answer");
    // Inspecting and collapsing a long record is deliberate reading intent.
    // Resume following explicitly before asserting that new output stays visible.
    if (await latestMessage.isVisible()) await latestMessage.click();
    await page.waitForFunction(() => {
      const status = document.querySelector(".msg__writing")?.getBoundingClientRect();
      const viewport = document.querySelector(".chatview__scroll")?.getBoundingClientRect();
      return status && viewport && status.top >= viewport.top && status.bottom <= viewport.bottom + 1;
    });
    await stage("complete");
    await process.getByText("本次处理已结束", { exact: true }).waitFor();
    assert.equal(await page.locator(".execution-process--running, .msg__writing").count(), 0);
    assert.equal(await header.getAttribute("aria-expanded"), "false");
    assert.match(await header.textContent(), /查看过程/);
    evidence.push({ width, height, theme, font, geometry });
  }
  assert.deepEqual(errors, []);
  writeFileSync(join(output, "evidence.json"), JSON.stringify({ layouts: evidence, errors }, null, 2));
  console.log(JSON.stringify({ ok: true, output, layouts: evidence.length,
    checks: ["default expansion", "answer width", "bounded records", "wheel and arrow keys", "follow and review", "restore reading after session switches", "distinct latest actions", "questions and approvals", "all operations", "details", "focus", "long analysis", "footer collapse returns to overview", "actual waiting", "answer feedback", "completion", "reduced motion"], errors }, null, 2));
} catch (error) {
  if (page) await page.screenshot({ path: join(output, "failure.png") }).catch(() => {});
  throw error;
} finally { await browser?.close(); await server.close(); }
