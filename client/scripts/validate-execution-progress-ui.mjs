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
  const errors = [];
  const evidence = [];
  const stage = async value => {
    await page.evaluate(async value => {
      const { useSessionStore } = await import("/src/stores/session-store.ts");
      const store = useSessionStore.getState();
      const tool = (id, title, status) => ({ kind: "tool_call", streamId: "stage-1", stageTitle: "核对资料与准备演示结构", toolCall: { toolCallId: id, kind: "read_file", title, status, content: [{ type: "text", text: "隔离资料内容，用于验证右侧详情正常打开。" }] } });
      const progress = { kind: "text", phase: "commentary", streamId: "stage-1", stageTitle: "核对资料与准备演示结构", text: "先核对资料来源与页面结构。\n\n正在整理养生建议和参考数据，接下来会把内容组织成清晰的演示页面。" };
      if (value === "long-thinking") progress.text = "较早的推理内容。".repeat(100) + "最新决定：改用备用方案验证。";
      if (value === "fenced-thinking") progress.text = "正在核对代码。\n\n```ts\n" + "const draft = 1;\n".repeat(300) + "```\n\n最新决定：代码已经核对完成。";
      const parts = value.includes("thinking") ? [progress] : [progress, { kind: "thought", text: "原始内部分析片段。", streamId: "stage-1" },
        tool("ongoing", "检查健康资料与参考文献", value === "tools" ? "in_progress" : "completed"),
        tool("old", "检查演示文稿规范", "completed"),
        tool("recent", "核对数据来源", "completed"),
        tool("latest", "读取页面组件与字体规范", "completed"),
      ];
      if (["answer", "long-answer", "complete"].includes(value)) parts.push({ kind: "text", phase: "final_answer", streamId: "answer-1", text: value === "long-answer"
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
    const toggle = page.locator(".execution-process__operations-toggle[aria-expanded=true]");
    if (await toggle.count()) await toggle.click();
  };
  const latestExcerptVisible = marker => page.waitForFunction(marker => {
    const text = document.querySelector(".execution-process__excerpt p");
    const start = text?.textContent.indexOf(marker) ?? -1;
    if (start < 0) return false;
    const range = document.createRange();
    range.setStart(text.firstChild, start); range.setEnd(text.firstChild, start + marker.length);
    const latest = range.getBoundingClientRect(), box = text.getBoundingClientRect();
    return latest.top >= box.top - 1 && latest.bottom <= box.bottom + 1
      && box.height <= parseFloat(getComputedStyle(text).lineHeight) + 1
      && getComputedStyle(text).whiteSpace === "nowrap";
  }, marker);

  for (const [width, height, theme, font] of [[1440, 1000, "light", 13], [1440, 1000, "dark", 13], [1024, 680, "light", 13], [1024, 680, "dark", 18]]) {
    await page?.close();
    const platform = width === 1440 ? "mac" : "windows";
    page = await browser.newPage({ reducedMotion: "reduce", userAgent: platform === "windows"
      ? "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36"
      : "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36" });
    page.setDefaultTimeout(15000);
    page.setDefaultNavigationTimeout(60000);
    page.on("pageerror", error => errors.push(error.message));
    await page.setViewportSize({ width, height });
    await page.goto(`http://127.0.0.1:${server.httpServer.address().port}/__ui-review?surface=execution&state=running&theme=${theme}`);
    await page.locator(".execution-process__header").waitFor();
    await page.evaluate(font => document.documentElement.style.fontSize = `${16 * font / 13}px`, font);
    await stage("thinking");
    assert.equal(await page.locator(".execution-process__header").getAttribute("aria-expanded"), "false");
    await page.getByText("核对资料与准备演示结构", { exact: true }).waitFor();
    assert.equal(await page.locator(".execution-process__reasoning").count(), 0, "thoughts stay behind disclosure");
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
    await page.getByLabel("当前阶段进展").getByText("正在整理养生建议和参考数据，接下来会把内容组织成清晰的演示页面。", { exact: true }).waitFor();
    assert.equal(await preview.locator(".toolcall").count(), 0, "tools start collapsed below the main stage");
    assert.equal(await page.getByText("原始内部分析片段。", { exact: true }).count(), 0);
    await page.screenshot({ path: join(output, `stage-${width}-${theme}-${font}.png`) });
    await preview.locator(".execution-process__operations-toggle").click();
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
    await preview.locator(".execution-process__operations-toggle").click();

    await page.locator(".execution-process__preview-more").click();
    assert(await page.locator(".execution-process__header").evaluate(element => element === document.activeElement));
    await page.locator(".execution-process__body .execution-process__operations-toggle").click();
    assert.equal(await page.locator(".execution-process__body .toolcall").count(), 4);
    assert.equal(await preview.count(), 0, "full history must not duplicate the preview");
    await page.locator(".execution-process__header").click();
    await preview.locator(".execution-process__operations-toggle").click();
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
    await page.evaluate(async () => {
      const { useSessionStore } = await import("/src/stores/session-store.ts");
      const { useQuestionStore } = await import("/src/stores/question-store.ts");
      useQuestionStore.getState().request({ requestId: "stage-question", sessionId: useSessionStore.getState().sessionId,
        toolCallId: "ongoing", title: "确认出行安排", mode: "default", questions: [{ id: "days", question: "计划游玩几天？",
          multiSelect: false, options: [{ label: "两天", description: "周末行程" }, { label: "三天", description: "更从容" }] }] });
    });
    await page.getByRole("button", { name: /等待你补充信息/ }).waitFor();
    const question = page.locator(".question-inline");
    await question.getByText("计划游玩几天？", { exact: true }).waitFor();
    assert.equal(await page.locator(".execution-process--running, .msg__writing, .execution-process .question-inline").count(), 0);
    await question.locator(".question-inline__option").first().click();
    await page.screenshot({ path: join(output, `waiting-${width}-${theme}-${font}.png`) });
    await question.getByRole("button", { name: "提交", exact: true }).click();
    await question.waitFor({ state: "hidden" });
    await page.locator(".execution-process--running").waitFor();
    await preview.locator(".execution-process__operations-toggle").click();
    await preview.locator('[data-tool-call-id="ongoing"]').focus();
    await stage("complete");
    assert(await page.locator(".execution-process__header").evaluate(element => element === document.activeElement), "completion must preserve keyboard navigation");
    assert.equal(await page.locator(".execution-process__header").getAttribute("aria-expanded"), "false");
    assert.equal(await page.locator(".execution-process--running, .execution-process__preview").count(), 0);
    assert.equal(await page.locator(".msg__writing").count(), 0);

    // Match the current Runtime wire format: public text has no phase, and a
    // UseTool call carries its effective identity in the title and raw input.
    const publicText = await page.evaluate(async () => {
      const { useSessionStore } = await import("/src/stores/session-store.ts");
      const store = useSessionStore.getState(), sessionId = store.sessionId;
      store.setMessages([]);
      store.startStreaming(sessionId, "wire-progress");
      const meta = { promptId: "wire-progress", streamStartMs: 7001 };
      const text = "已核实交通方案：乘坐高铁前往南京。车程约两小时，以下内容在后续工具调用和任务中止后仍应完整保留。";
      store.applyUpdate({ __sessionId: sessionId, sessionUpdate: "agent_message_chunk", content: { type: "text", text }, _meta: meta });
      store.applyUpdate({ __sessionId: sessionId, sessionUpdate: "tool_call", toolCallId: "wire-weather", kind: "other",
        title: "echoagent-live-info__weather_forecast", status: "in_progress", rawInput: { variant: "UseTool", tool_name: "echoagent-live-info__weather_forecast", tool_input: { location: "南京" } },
        _meta: { ...meta, "echo.agent/tool": { version: 1, name: "use_tool" } } });
      return text;
    });
    await page.getByRole("button", { name: /^正在查询天气(?: ·.*)?$/ }).waitFor();
    await page.getByLabel("当前阶段进展").waitFor();
    assert.equal(await page.getByText(publicText, { exact: true }).count(), 0, "full source text belongs to its stage; only the latest sentence is previewed");
    assert.equal(await page.locator(".execution-process__excerpt").count(), 1);
    assert.equal(await page.locator(".msg__writing").count(), 0);
    await page.getByRole("button", { name: "1 项进行中", exact: true }).click();
    await page.getByText("🌤️ 天气查询", { exact: true }).waitFor();
    await page.getByRole("button", { name: "other 南京 运行中", exact: true }).waitFor();
    const wireExport = await page.evaluate(async () => {
      const { useSessionStore } = await import("/src/stores/session-store.ts");
      const { buildSessionMarkdown } = await import("/src/lib/export-markdown.ts");
      const store = useSessionStore.getState();
      store.applyUpdate({ __sessionId: store.sessionId, sessionUpdate: "turn_completed", prompt_id: "wire-progress", stop_reason: "cancelled" });
      return buildSessionMarkdown(useSessionStore.getState().messages);
    });
    assert(wireExport.includes(publicText), "cancellation keeps phase-less public text in normal exports");
    await page.getByRole("button", { name: /^查看过程(?: ·.*)?$/ }).waitFor();
    assert.equal(await page.getByText(publicText, { exact: true }).count(), 0);
    await page.locator(".execution-process__header").click();
    await page.locator(".execution-process__progress").getByText(publicText, { exact: true }).waitFor();
    await page.locator(".execution-process__header").click();
    await page.screenshot({ path: join(output, `wire-result-${width}-${theme}-${font}.png`) });

    // Optional explicit commentary metadata must preserve the order of the
    // underlying events when several generations share a real plan stage.
    await page.evaluate(async () => {
      const { useSessionStore } = await import("/src/stores/session-store.ts");
      const store = useSessionStore.getState(), sessionId = store.sessionId;
      store.setMessages([]);
      store.startStreaming(sessionId, "ordered-progress");
      const emit = (sessionUpdate, fields, streamStartMs = 8001) => store.applyUpdate({ __sessionId: sessionId, sessionUpdate, ...fields, _meta: { promptId: "ordered-progress", streamStartMs } });
      emit("plan", { entries: [{ content: "核验结果并修复", priority: "high", status: "in_progress" }] });
      emit("agent_thought_chunk", { content: { type: "text", text: "检查原配置。" } });
      emit("tool_call", { toolCallId: "first", title: "首次读取失败", kind: "read", status: "failed" });
      emit("agent_message_chunk", { content: { type: "text", text: "改用备用配置。", _meta: { phase: "commentary" } } }, 8002);
      emit("tool_call", { toolCallId: "retry", title: "读取备用配置", kind: "read", status: "in_progress" }, 8002);
    });
    await page.getByRole("button", { name: /^核验结果并修复(?: ·.*)?$/ }).click();
    const ordered = page.locator(".execution-process__stage");
    assert.equal(await ordered.count(), 1);
    assert.deepEqual(await ordered.evaluate(element => [...element.children]
      .filter(child => child.matches(".execution-process__reasoning, .execution-process__progress, .execution-process__tool"))
      .map(child => child.querySelector("[data-tool-call-id]")?.dataset.toolCallId ?? (child.matches("details") ? "thought" : child.textContent))),
      ["thought", "first", "改用备用配置。", "retry"]);
    assert.equal(await ordered.locator("details[open]").count(), 0);
    await page.screenshot({ path: join(output, `ordered-stage-${width}-${theme}-${font}.png`) });

    // Replay the reference video's structure: public stage commentary,
    // associated searches/commands, optional reasoning and a changing live line.
    await page.evaluate(async () => {
      const { useSessionStore } = await import("/src/stores/session-store.ts");
      const store = useSessionStore.getState(), sessionId = store.sessionId;
      store.setMessages([]); store.startStreaming(sessionId, "travel-stages");
      const parts = [];
      const descriptions = ["先核对泰山官方门票、索道和交通信息。", "信息够了，再补齐当前季节的关键细节。", "信息齐了，先整理路线全景，再生成完整攻略文件。"];
      descriptions.forEach((text, index) => {
        const streamId = `travel-${index}`;
        parts.push({ kind: "text", text, streamId });
        parts.push({ kind: "thought", text: "核对来源、适用日期与路线条件，保留不确定的信息。", streamId });
        for (let call = 0; call < 20; call++) parts.push({ kind: "tool_call", streamId, toolCall: {
          toolCallId: `${streamId}-${call}`, title: index === 2 ? `整理攻略文件 · ${call + 1}` : `核对官网资料 · ${call + 1}`,
          kind: index === 2 ? "bash" : "web_search", status: index === 2 && call === 19 ? "in_progress" : "completed", content: [],
        } });
      });
      store.setMessages([
        { id: "travel-user", role: "user", complete: true, parts: [{ kind: "text", text: "帮我做一份泰山游玩攻略。" }] },
        { id: useSessionStore.getState().streamingMessageId, role: "assistant", complete: false, startedAt: Date.now() - 373_000, parts },
      ]);
    });
    const liveLine = page.getByLabel("当前阶段进展");
    await liveLine.getByText("信息齐了，先整理路线全景，再生成完整攻略文件。", { exact: true }).waitFor();
    assert.equal(await page.locator(".execution-process__excerpt").count(), 1);
    assert.equal(await page.locator(".msg__body > .markdown-body").count(), 0, "intermediate generations must not accumulate as answers");
    const lineGeometry = await liveLine.locator("p").evaluate(element => ({ height: element.clientHeight, line: parseFloat(getComputedStyle(element).lineHeight), whiteSpace: getComputedStyle(element).whiteSpace }));
    assert(lineGeometry.height <= lineGeometry.line + 1 && lineGeometry.whiteSpace === "nowrap");
    await page.locator(".msg--assistant").screenshot({ path: join(output, `travel-compact-${width}-${theme}-${font}.png`) });
    await page.locator(".execution-process__header").click();
    const history = page.getByRole("region", { name: "思考与执行过程" });
    await history.waitFor();
    assert.equal(await history.locator(".execution-process__stage").count(), 3);
    assert.equal(await history.locator(".execution-process__tool-group").count(), 3);
    assert.equal(await history.locator(".toolcall").count(), 0, "long batches start folded");
    assert.equal(await page.locator(".execution-process__stage--current").getAttribute("data-stage-id"), await liveLine.getAttribute("data-stage-id"));
    await history.locator(".execution-process__operations-toggle").last().click();
    assert.equal(await history.locator(".toolcall").count(), 20);
    await history.evaluate(element => { element.scrollTop = 0; });
    await page.waitForFunction(() => document.querySelector(".execution-process__body").scrollTop === 0);
    await page.evaluate(async () => {
      const { useSessionStore } = await import("/src/stores/session-store.ts");
      const store = useSessionStore.getState();
      store.setMessages(store.messages.map(message => message.role === "assistant" ? { ...message, parts: [...message.parts,
        { kind: "text", streamId: "travel-2", text: "路线结构已整理，正在核对攻略文件。" },
      ] } : message));
    });
    await liveLine.getByText("路线结构已整理，正在核对攻略文件。", { exact: true }).waitFor();
    assert.equal(await history.evaluate(element => element.scrollTop), 0, "streaming must not interrupt reading older stages");
    const historyGeometry = await history.evaluate(element => ({ height: element.clientHeight, scrollHeight: element.scrollHeight }));
    assert(historyGeometry.scrollHeight > historyGeometry.height && historyGeometry.height <= height * .52 + 1);
    await page.locator(".msg--assistant").screenshot({ path: join(output, `travel-expanded-${width}-${theme}-${font}.png`) });
    await history.locator(".execution-process__operations-toggle").last().click();
    await page.locator(".msg--assistant").screenshot({ path: join(output, `travel-stages-${width}-${theme}-${font}.png`) });
    evidence.push({ platform, width, height, theme, font, geometry });
  }
  assert.deepEqual(errors, []);
  writeFileSync(join(output, "evidence.json"), JSON.stringify({ evidence, errors }, null, 2));
  console.log(JSON.stringify({ ok: true, output, layouts: evidence.length, checks: ["primary stage description", "collapsed tool groups", "Mac/Windows browser platforms", "single current-stage line", "font and width changes", "long code fences", "bounded parallel activity", "tool drawer", "keyboard focus", "manual disclosure", "independent question and resume", "long-answer feedback", "completion", "phase-less Runtime text and cancellation export", "real UseTool weather identity", "chronological plan events", "reduced motion"], errors }, null, 2));
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
