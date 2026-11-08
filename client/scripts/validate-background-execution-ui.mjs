import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createUiReviewServer, reviewChromium, reviewOutput } from "./ui-review-runtime.mjs";

const output = reviewOutput(mkdtempSync(join(tmpdir(), "echo-background-execution-")));
const server = await createUiReviewServer(0);
let browser;
try {
  browser = await (await reviewChromium()).launch({ headless: true, executablePath: process.env.CHROME_BIN || "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" });
  const page = await browser.newPage({ viewport: { width: 1280, height: 860 } });
  const errors = [];
  page.on("pageerror", error => errors.push(error.message));
  await page.goto(`http://127.0.0.1:${server.httpServer.address().port}/__ui-review?surface=workbench&platform=win&continuation=1&state=running`);
  await page.locator(".echo-composer__input").waitFor();
  await page.locator(".echo-composer__input").fill("保留中的下一条消息");
  await page.evaluate(async () => {
    const { useSessionStore } = await import("/src/stores/session-store.ts");
    const { useSessionsStore } = await import("/src/stores/sessions-store.ts");
    const state = useSessionStore.getState();
    const sessionId = state.sessionId;
    const invoke = window.__TAURI_INTERNALS__.invoke;
    window.__TAURI_INTERNALS__.invoke = (command, args) => command === "tasks_list" && args?.sessionId === sessionId
      ? Promise.resolve([{ id: "preview-service", source: "task", description: "本地预览服务", status: "running", sessionId }]) : invoke(command, args);
    state.setMessages([]);
    state.pushUser("排查微信输入框字体");
    state.startStreaming(sessionId, "video-human");
    const emit = (sessionUpdate, fields = {}, meta = {}) => useSessionStore.getState().applyUpdate({ sessionUpdate, ...fields, __sessionId: sessionId, _meta: meta });
    emit("user_message_chunk", { content: { type: "text", text: "排查微信输入框字体" } }, { promptId: "video-human", promptIndex: 0, turnOrigin: { origin: "user" } });
    // Production delivers the source and tool events on separate rails.
    emit("task_backgrounded", { task_id: "directory-task", tool_call_id: "directory", description: "检查微信数据目录" });
    emit("tool_call", { toolCallId: "directory", title: "检查微信数据目录", kind: "read", status: "completed" }, { promptId: "video-human" });
    emit("agent_message_chunk", { content: { type: "text", text: "已取得字体排查结论，临时改动已还原。\n\n" + Array.from({ length: 18 }, (_, i) => `排查依据 ${i + 1}：已核对输入框、显示缩放与应用设置，保留现有环境中的可验证结果。`).join("\n\n") } }, { promptId: "video-human" });
    state.markComplete({ sessionId, promptId: "video-human", stopReason: "end_turn" });
    useSessionsStore.getState().upsert({ sessionId, status: "completed" });
    window.backgroundReview = { emit, sessionId };
  });
  const original = page.locator(".msg-wrap").filter({ hasText: "已取得字体排查结论" });
  const branch = page.getByRole("menuitem", { name: "在新对话中继续", exact: true });
  await page.getByRole("button", { name: "后台任务 1", exact: true }).click();
  await page.getByText("本地预览服务", { exact: true }).waitFor();
  assert.equal(await page.getByRole("button", { name: "终止任务：本地预览服务" }).isEnabled(), true);
  assert.equal(await page.getByLabel("整体任务状态", { exact: true }).count(), 0);
  assert.equal(await page.evaluate(async () => {
    const { useSessionsStore } = await import("/src/stores/sessions-store.ts");
    return useSessionsStore.getState().independent.find(row => row.sessionId === window.backgroundReview.sessionId).status;
  }), "completed", "querying a persistent background service must not overwrite session lifecycle");
  await page.getByRole("button", { name: "关闭面板", exact: true }).click();
  await original.getByRole("button", { name: "更多回复操作" }).click();
  assert.equal(await branch.isDisabled(), false, "a persistent service must not prevent history branching");
  await page.keyboard.press("Escape");
  await page.screenshot({ path: join(output, "completed-with-service.png") });
  // Preserve the reader's position while an automatic continuation is admitted.
  const readingTop = await page.evaluate(async () => {
    const scroll = document.querySelector(".chatview__scroll");
    scroll.scrollTop = 100;
    scroll.dispatchEvent(new Event("scroll"));
    await new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)));
    return scroll.scrollTop;
  });
  await page.evaluate(() => {
    const { emit } = window.backgroundReview;
    emit("user_message_chunk", { content: { type: "text", text: "PRIVATE INTERNAL REMINDER" } }, {
      promptId: "task-completed-directory-task", promptIndex: 1, hideFromScrollback: true,
      turnOrigin: { origin: "background_task", sourceId: "directory-task" },
    });
    emit("agent_message_chunk", { content: { type: "text", text: "正在检查后台结果是否影响已有结论。", _meta: { phase: "commentary" } } }, { promptId: "task-completed-directory-task" });
    emit("tool_call", { toolCallId: "verify", title: "核对后台检查结果", kind: "read", status: "in_progress" }, { promptId: "task-completed-directory-task" });
  });
  await page.getByLabel("本次处理说明", { exact: true }).waitFor();
  const readingAfter = await page.locator(".chatview__scroll").evaluate(el => el.scrollTop);
  assert.ok(Math.abs(readingAfter - readingTop) < 2, "automatic continuation must preserve the reader's position");
  assert.match(await page.getByLabel("本次处理说明").textContent(), /后台结果核对.*检查微信数据目录/s);
  assert.equal(await page.getByText("PRIVATE INTERNAL REMINDER").count(), 0);
  await original.getByRole("button", { name: "更多回复操作" }).click();
  assert.equal(await branch.isDisabled(), true, "actual model work keeps the existing history guard");
  assert.match(await branch.getAttribute("title"), /当前执行/);
  await page.keyboard.press("Escape");
  await page.getByLabel("本次处理说明").scrollIntoViewIfNeeded();
  await page.screenshot({ path: join(output, "followup-running.png") });
  await page.evaluate(async () => {
    const { emit, sessionId } = window.backgroundReview;
    emit("tool_call_update", { toolCallId: "verify", status: "completed" }, { promptId: "task-completed-directory-task" });
    emit("agent_message_chunk", { content: { type: "text", text: "后台目录检查已完成，原结论不变，无需再操作。", _meta: { phase: "final_answer" } } }, { promptId: "task-completed-directory-task" });
    const { useSessionStore } = await import("/src/stores/session-store.ts");
    useSessionStore.getState().markComplete({ sessionId, promptId: "task-completed-directory-task", stopReason: "end_turn" });
  });
  const supplement = page.locator(".msg-wrap").filter({ hasText: "后台目录检查已完成" });
  await supplement.getByRole("button", { name: "更多回复操作" }).waitFor();
  assert.equal(await supplement.getByRole("button", { name: /重新执行|重新生成|重试回复/ }).count(), 0);
  await original.getByRole("button", { name: "更多回复操作" }).click();
  assert.equal(await branch.isDisabled(), false);
  await page.keyboard.press("Escape");
  const targets = await page.evaluate(async () => {
    const { useSessionStore } = await import("/src/stores/session-store.ts");
    const { continuationTargets, continuationAnchor } = await import("/src/lib/session-continuation.ts");
    const messages = useSessionStore.getState().messages;
    return [...continuationTargets(messages)].map(([id, target]) => ({ target, anchored: continuationAnchor(messages, target) === id }));
  });
  assert.deepEqual(targets, [
    { target: { promptIndex: 0, promptId: "video-human" }, anchored: true },
    { target: { promptIndex: 1, promptId: "task-completed-directory-task" }, anchored: true },
  ]);
  // A late result of the older request stays explicit after a new human turn.
  await page.evaluate(() => {
    const { emit } = window.backgroundReview;
    emit("user_message_chunk", { content: { type: "text", text: "现在检查通知设置" } }, { promptId: "new-question", promptIndex: 2 });
    emit("agent_message_chunk", { content: { type: "text", text: "通知设置已检查。" } }, { promptId: "new-question" });
    emit("turn_completed", { prompt_id: "new-question", stop_reason: "end_turn" });
    emit("user_message_chunk", { content: { type: "text", text: "PRIVATE INTERNAL REMINDER" } }, {
      promptId: "late-native-wake", promptIndex: 3, hideFromScrollback: true, turnOrigin: { origin: "background_task", sourceId: "directory-task" },
    });
    emit("agent_message_chunk", { content: { type: "text", text: "旧目录结果复核无变化。" } }, { promptId: "late-native-wake" });
    emit("turn_completed", { prompt_id: "late-native-wake", stop_reason: "end_turn" });
  });
  const late = page.locator(".msg-wrap").filter({ hasText: "旧目录结果复核无变化" });
  await late.waitFor();
  assert.match(await late.getByLabel("本次处理说明").textContent(), /来自较早的请求：排查微信输入框字体/);
  assert.equal(await page.locator(".echo-composer__input").inputValue(), "保留中的下一条消息");
  await late.scrollIntoViewIfNeeded();
  await page.screenshot({ path: join(output, "settled.png") });
  await page.setViewportSize({ width: 1024, height: 760 });
  await page.evaluate(() => { document.documentElement.dataset.theme = "dark"; document.documentElement.style.fontSize = "20px"; });
  const geometry = await page.evaluate(() => ({ overflow: document.documentElement.scrollWidth > innerWidth + 1,
    sourceOverflow: [...document.querySelectorAll(".execution-turn")].some(el => el.scrollWidth > el.clientWidth + 1) }));
  assert.deepEqual(geometry, { overflow: false, sourceOverflow: false });
  await late.scrollIntoViewIfNeeded();
  await page.screenshot({ path: join(output, "dark-large-text.png") });
  // Default continuation must preserve a newer failed request even without a
  // public answer, while keeping the first optimistic message visible in child.
  await page.evaluate(() => {
    const { emit } = window.backgroundReview;
    emit("user_message_chunk", { content: { type: "text", text: "最新约束必须保留" } }, { promptId: "failed-latest", promptIndex: 4 });
    emit("agent_thought_chunk", { content: { type: "text", text: "连接失败前已记录的分析" } }, { promptId: "failed-latest" });
    emit("turn_completed", { prompt_id: "failed-latest", stop_reason: "error" });
  });
  await page.getByRole("button", { name: "任务操作", exact: true }).click();
  await branch.click();
  await page.waitForFunction(async () => {
    const { useSessionStore } = await import("/src/stores/session-store.ts");
    return useSessionStore.getState().sessionId !== window.backgroundReview.sessionId;
  });
  const continued = await page.evaluate(async () => {
    const { useSessionStore } = await import("/src/stores/session-store.ts");
    const state = useSessionStore.getState();
    const request = window.workbenchCalls.filter(call => call.command === "session_continue").at(-1)?.args;
    return { request, retainedLatest: state.messages.some(message => message.role === "user" && message.promptId === "failed-latest") };
  });
  assert.equal(continued.request.targetPromptIndex, 4);
  assert.equal(continued.request.sourcePromptId, "failed-latest");
  assert.equal(continued.retainedLatest, true);
  await page.evaluate(async () => {
    const { useSessionStore } = await import("/src/stores/session-store.ts");
    const state = useSessionStore.getState();
    state.pushUser("新方向的第一条消息");
    state.startStreaming(state.sessionId, "child-first");
  });
  await page.getByText("新方向的第一条消息", { exact: true }).waitFor();
  assert.equal(await page.locator("#conversation-inherited-history").isVisible(), false);
  await page.screenshot({ path: join(output, "continued-after-failure.png") });
  assert.deepEqual(errors, []);
  writeFileSync(join(output, "evidence.json"), JSON.stringify({ targets, geometry, continued, readingTop, readingAfter, errors }, null, 2));
  console.log(`Passed reordered source, persistent service, failed-turn continuation and reading continuity: ${output}`);
} finally { await browser?.close(); await server.close(); }
