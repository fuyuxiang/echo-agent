// Isolated production ChatView; all model/tool events below are fixture data.
import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createUiReviewServer, reviewChromium, reviewOutput } from "./ui-review-runtime.mjs";

const output = reviewOutput(mkdtempSync(join(tmpdir(), "echo-execution-progress-")));
const server = await createUiReviewServer(0, { freezeMotion: false });
let browser, page;
try {
  browser = await (await reviewChromium()).launch({ ...(process.env.CHROME_BIN ? { executablePath: process.env.CHROME_BIN }
    : process.platform === "darwin" ? { executablePath: "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" } : {}), headless: true });
  page = await browser.newPage({ reducedMotion: "reduce" });
  page.setDefaultTimeout(15000);
  page.setDefaultNavigationTimeout(60000);
  const errors = [];
  page.on("pageerror", error => errors.push(error.message));
  const evidence = [];
  const stage = async value => {
    await page.evaluate(async value => {
      const { useSessionStore } = await import("/src/stores/session-store.ts");
      const store = useSessionStore.getState();
      const tool = (id, title, status) => ({ kind: "tool_call", toolCall: { toolCallId: id, kind: "read_file", title, status, content: [{ type: "text", text: "隔离资料内容，用于验证右侧详情正常打开。" }] } });
      const thought = { kind: "thought", text: "先核对资料来源与页面结构。\n\n正在整理养生建议和参考数据，接下来会把内容组织成清晰的演示页面。" };
      if (value === "long-thinking") thought.text = "较早的推理内容。".repeat(100) + "最新决定：改用备用方案验证。";
      if (value === "fenced-thinking") thought.text = "正在核对代码。\n\n```ts\n" + "const draft = 1;\n".repeat(300) + "```\n\n最新决定：代码已经核对完成。";
      const parts = value.includes("thinking") ? [thought] : [thought,
        tool("ongoing", "检查健康资料与参考文献", value === "tools" ? "in_progress" : "completed"),
        tool("old", "检查演示文稿规范", "completed"),
        tool("recent", "核对数据来源", "completed"),
        tool("latest", "读取页面组件与字体规范", "completed"),
      ];
      if (["answer", "long-answer", "complete"].includes(value)) parts.push({ kind: "text", text: value === "long-answer"
        ? Array.from({ length: 30 }, (_, index) => `第 ${index + 1} 段：整理办公场景中的日常健康建议，保留依据并说明适用条件。`).join("\n\n")
        : "资料已经整理好。演示文稿将采用清晰的章节结构，突出适合办公场景的日常健康建议。" });
      const complete = value === "complete";
      store.setMessages([
        { id: "progress-user", role: "user", complete: true, parts: [{ kind: "text", text: "做一份面向办公室人群的健康养生演示文稿。" }] },
        { id: store.streamingMessageId, role: "assistant", complete, startedAt: Date.now() - 76_000, completedAt: complete ? Date.now() : undefined, stopReason: complete ? "end_turn" : undefined, parts },
      ]);
    }, value);
    await page.waitForFunction(value => {
      const header = document.querySelector(".execution-process__header");
      return header && (value === "complete" ? header.textContent.includes("查看过程") : value.includes("answer") ? document.querySelector(".msg__writing") : document.querySelector(".execution-process__preview"));
    }, value);
  };
  const latestExcerptVisible = marker => page.waitForFunction(marker => {
    const text = document.querySelector(".execution-process__excerpt p");
    const start = text?.textContent.indexOf(marker) ?? -1;
    if (start < 0) return false;
    const range = document.createRange();
    range.setStart(text.firstChild, start); range.setEnd(text.firstChild, start + marker.length);
    const latest = range.getBoundingClientRect(), box = text.getBoundingClientRect();
    return latest.top >= box.top - 1 && latest.bottom <= box.bottom + 1
      && box.height <= parseFloat(getComputedStyle(text).lineHeight) * 2 + 1;
  }, marker);

  for (const [width, height, theme, font] of [[1440, 1000, "light", 13], [1440, 1000, "dark", 13], [1024, 680, "light", 13], [1024, 680, "dark", 18]]) {
    await page.setViewportSize({ width, height });
    await page.goto(`http://127.0.0.1:${server.httpServer.address().port}/__ui-review?surface=execution&state=running&theme=${theme}`);
    await page.locator(".execution-process__header").waitFor();
    await page.evaluate(font => document.documentElement.style.fontSize = `${16 * font / 13}px`, font);
    await stage("thinking");
    assert.equal(await page.locator(".execution-process__header").getAttribute("aria-expanded"), "false");
    await page.getByText("思考片段", { exact: true }).waitFor();
    await page.screenshot({ path: join(output, `thinking-${width}-${theme}-${font}.png`) });

    await stage("long-thinking");
    await latestExcerptVisible("改用备用方案验证。");
    // Resize without a React update: the existing excerpt must keep following.
    await page.setViewportSize({ width: width === 1440 ? 1024 : 1280, height });
    await page.evaluate(font => document.documentElement.style.fontSize = `${16 * font / 13}px`, font === 13 ? 18 : 13);
    await latestExcerptVisible("改用备用方案验证。");
    await page.setViewportSize({ width, height });
    await page.evaluate(font => document.documentElement.style.fontSize = `${16 * font / 13}px`, font);
    await latestExcerptVisible("改用备用方案验证。");
    await page.screenshot({ path: join(output, `latest-excerpt-${width}-${theme}-${font}.png`) });
    await stage("fenced-thinking");
    await page.getByText("最新决定：代码已经核对完成。", { exact: true }).waitFor();
    assert(!(await page.locator(".execution-process__excerpt p").textContent()).includes("const draft"));

    await stage("tools");
    const preview = page.locator(".execution-process__preview");
    assert.equal(await preview.locator(".toolcall").count(), 3);
    assert.equal(await preview.locator(".toolcall--run").count(), 1, "keep earlier parallel work visible");
    await preview.getByText("运行中", { exact: true }).waitFor();
    await preview.locator(".toolcall--run").click();
    await page.getByText("隔离资料内容，用于验证右侧详情正常打开。", { exact: true }).waitFor();
    await page.getByRole("button", { name: "关闭面板", exact: true }).click();
    const geometry = await preview.evaluate(element => {
      const box = element.getBoundingClientRect();
      return { x: box.x, right: box.right, height: box.height, clipped: element.scrollWidth > element.clientWidth + 1, overflow: document.documentElement.scrollWidth > innerWidth + 1, animations: element.getAnimations({ subtree: true }).length };
    });
    assert(!geometry.clipped && !geometry.overflow && geometry.x >= 0 && geometry.right <= width + 1, "activity must fit the transcript");
    assert(geometry.height < 300, "preview must remain bounded as history grows");
    assert.equal(geometry.animations, 0, "honor reduced motion");
    assert.equal(await page.locator(".msg__caret, .msg__writing").count(), 0, "tool execution must not show an answer cursor");
    await page.screenshot({ path: join(output, `tools-${width}-${theme}-${font}.png`) });
    await page.locator(".msg--assistant").screenshot({ path: join(output, `progress-${width}-${theme}-${font}.png`) });

    const focusedTool = preview.locator('[data-tool-call-id="ongoing"]');
    await focusedTool.focus();
    await page.evaluate(async () => {
      const { useSessionStore } = await import("/src/stores/session-store.ts");
      const store = useSessionStore.getState();
      store.setMessages(store.messages.map(message => ({ ...message, parts: message.parts.map(part =>
        part.kind === "tool_call" && part.toolCall.toolCallId === "ongoing" ? { ...part, toolCall: { ...part.toolCall, status: "completed" } } : part) })));
    });
    await focusedTool.getByText("完成", { exact: true }).waitFor();
    assert(await focusedTool.evaluate(element => element === document.activeElement), "completion must retain the keyboard target");
    assert.equal(await preview.locator(".toolcall").count(), 3);
    await page.locator(".execution-process__header").focus();
    assert.equal(await focusedTool.count(), 0, "leaving the target restores the recent-call selection");
    await stage("tools");

    await page.locator(".execution-process__preview-more").click();
    assert.equal(await page.locator(".execution-process__body .toolcall").count(), 4);
    assert.equal(await preview.count(), 0, "full history must not duplicate the preview");
    assert(await page.locator(".execution-process__header").evaluate(element => element === document.activeElement));
    await page.locator(".execution-process__header").click();
    await preview.locator('[data-tool-call-id="ongoing"]').focus();
    await stage("answer");
    assert.equal(await preview.count(), 0, "answer generation retires the live preview");
    assert(await page.locator(".execution-process__header").evaluate(element => element === document.activeElement), "answer transition must return focus to the process entry");
    assert.equal(await page.locator(".msg__writing[role=status]").count(), 1, "the answer tail keeps its own generation feedback");
    assert.equal(await page.locator(".msg--assistant [role=status]").count(), 1, "do not announce answer generation twice");
    await page.screenshot({ path: join(output, `answer-${width}-${theme}-${font}.png`) });
    await stage("long-answer");
    await page.waitForFunction(() => {
      const status = document.querySelector(".msg__writing")?.getBoundingClientRect(), viewport = document.querySelector(".chatview__scroll")?.getBoundingClientRect();
      const header = document.querySelector(".execution-process__header")?.getBoundingClientRect();
      return status && viewport && header && status.top >= viewport.top && status.bottom <= viewport.bottom + 1 && header.bottom < viewport.top;
    });
    await page.screenshot({ path: join(output, `long-answer-${width}-${theme}-${font}.png`) });
    await stage("tools");
    await preview.locator('[data-tool-call-id="ongoing"]').focus();
    await stage("complete");
    assert(await page.locator(".execution-process__header").evaluate(element => element === document.activeElement), "completion must preserve keyboard navigation");
    assert.equal(await page.locator(".execution-process__header").getAttribute("aria-expanded"), "false");
    assert.equal(await page.locator(".execution-process--running, .execution-process__preview").count(), 0);
    assert.equal(await page.locator(".msg__writing").count(), 0);
    evidence.push({ width, height, theme, font, geometry });
  }
  assert.deepEqual(errors, []);
  writeFileSync(join(output, "evidence.json"), JSON.stringify({ evidence, errors }, null, 2));
  console.log(JSON.stringify({ ok: true, output, layouts: evidence.length, checks: ["latest two lines", "font and width changes", "long code fences", "bounded parallel activity", "tool drawer", "keyboard focus", "manual disclosure", "long-answer feedback", "completion", "reduced motion"], errors }, null, 2));
} catch (error) {
  await page?.screenshot({ path: join(output, "failure.png") });
  console.error(await page?.evaluate(async () => {
    const { useSessionStore } = await import("/src/stores/session-store.ts");
    const store = useSessionStore.getState();
    return { sessionId: store.sessionId, streaming: store.streaming, messages: store.messages.map(message => ({ id: message.id, complete: message.complete, kinds: message.parts.map(part => part.kind) })), header: document.querySelector(".execution-process__header")?.textContent, text: document.querySelector(".msg--assistant")?.textContent };
  }));
  console.error(`Failure evidence: ${output}`);
  throw error;
} finally { await browser?.close(); await server.close(); }
