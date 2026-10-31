import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createUiReviewServer, reviewChromium, reviewOutput } from "./ui-review-runtime.mjs";

const output = reviewOutput(mkdtempSync(join(tmpdir(), "echo-execution-outcome-")));
console.log(`Execution outcome evidence: ${output}`);
const server = await createUiReviewServer(0);
const base = `http://127.0.0.1:${server.httpServer.address().port}/__ui-review?surface=workbench&state=empty&platform=mac`;
let browser, page;
const evidence = [];
try {
  browser = await (await reviewChromium()).launch({ headless: true, executablePath: process.env.CHROME_BIN || "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" });
  page = await browser.newPage();
  page.setDefaultTimeout(15000); page.setDefaultNavigationTimeout(60000);
  const errors = []; page.on("pageerror", error => errors.push(error.message));
  const setup = scenario => page.evaluate(async scenario => {
    const { useSessionStore } = await import("/src/stores/session-store.ts");
    const store = useSessionStore.getState(), sid = "review-conversation";
    store.clearReplaySuppression(sid);
    store.pushUser("上一轮问题"); store.pushAssistant("上一轮已回复。"); store.pushUser("查询资料并生成报告");
    if (scenario !== "history") store.startStreaming(sid, "outcome-review");
    const timestamp = Date.now() - 5000;
    const emit = (kind, payload, offset, generation) => store.applyUpdate({ sessionUpdate: kind, __sessionId: sid, ...payload, _meta: { agentTimestampMs: timestamp + offset, ...(scenario === "history" ? { isReplay: true } : { promptId: "outcome-review" }), ...(generation ? { streamStartMs: generation } : {}), ...(scenario.includes("browser-upload") && payload.toolCallId === "query-2" ? {"echo.agent/tool":{version:1,name:"echoagent-browser__browser_upload"}} : {}) } });
    const firstFails = ["recovered", "alternate", "failed", "all-failed"].includes(scenario);
    emit("tool_call", { toolCallId: "query-1", kind: "web_fetch", title: "查询资料", rawInput: { city: "北京", source: "weather" }, status: "in_progress", content: [] }, 0, "weather-query");
    emit("tool_call_update", { toolCallId: "query-1", status: firstFails ? "failed" : "completed", content: [{ type: "text", text: firstFails ? "首次连接超时" : "资料查询成功" }] }, 1000, "weather-query");
    const retry = ["recovered", "failed", "history", "alternate"].includes(scenario);
    const kind = scenario.includes("browser-upload") ? "other" : scenario === "uncertain-send" ? "send_message" : retry ? "web_fetch" : "write_file";
    const rawInput = retry ? { city: "北京", source: scenario === "alternate" ? "another-source" : "weather" } : { path: "/review/report.md", content: "报告" };
    emit("tool_call", { toolCallId: "query-2", kind, title: retry ? "查询资料" : "生成报告", rawInput, status: "in_progress", content: [] }, 2000, retry ? "weather-query-retry" : "write-report");
    if (!["unfinished", "uncertain-send", "uncertain-browser-upload"].includes(scenario)) emit("tool_call_update", { toolCallId: "query-2", status: retry ? "completed" : "failed", content: [{ type: "text", text: retry ? "已取得查询结果" : "报告目录无写入权限" }] }, 3000, retry ? "weather-query-retry" : "write-report");
    emit("agent_message_chunk", { content: { type: "text", text: "这里保留已经整理好的查询结果。" } }, 4000);
  }, scenario);
  const settle = scenario => page.evaluate(async scenario => {
    const { useSessionStore } = await import("/src/stores/session-store.ts");
    if (scenario === "history") { useSessionStore.getState().finalizeIncompleteReplay("review-conversation"); return; }
    useSessionStore.getState().markComplete({ sessionId: "review-conversation", promptId: "outcome-review", stopReason: scenario === "failed" ? "error" : "end_turn", ...(scenario === "failed" ? { agentResult: "连接中断，最终检查未完成" } : {}) });
    if (scenario === "failed") useSessionStore.getState().setError("连接中断，最终检查未完成", {sessionId:"review-conversation",promptId:"outcome-review"});
  }, scenario);

  for (const [width, height, theme] of [[1440, 900, "light"], [1280, 800, "dark"], [1024, 768, "light"], [390, 844, "dark"]]) {
    for (const scenario of ["recovered", "alternate", "partial", "failed"]) {
      console.log(`Checking ${scenario} ${width} ${theme}`);
      await page.setViewportSize({ width, height }); await page.goto(`${base}&theme=${theme}`);
      await page.locator(".session-header").waitFor();
      if (width < 720) await page.getByRole("button", { name: "收起侧边栏", exact: true }).click();
      await setup(scenario);
      const process = page.locator(".execution-process"), header = process.locator(".execution-process__header");
      await page.locator(".execution-process--running").waitFor();
      assert.doesNotMatch(await header.textContent(), /已完成|查看过程/);
      assert.equal(await header.getAttribute("aria-expanded"), "false");
      assert.equal(await page.locator(".session-header [role=status]").count(), 0);
      await settle(scenario);
      await page.waitForFunction(() => !document.querySelector(".execution-process--running"));
      assert.equal(await header.getAttribute("aria-expanded"), "false");
      assert.equal(await process.locator(".toolcall, .execution-process__outcome-detail").count(), 0);
      assert.doesNotMatch(await header.textContent(), /部分|失败|项操作|待确认/);
      assert.equal(await page.locator(".session-header [role=status]").count(), 0);
      const hasIssue = ["partial", "failed"].includes(scenario);
      assert.equal(await page.locator(".execution-notice").count(), hasIssue ? 1 : 0);
      assert.equal(await page.locator(".chatview__error-banner").count(), 0, "a terminal failure must have only one presentation");
      if (hasIssue) {
        const notice = page.locator(".execution-notice");
        assert((await notice.textContent()).includes(scenario === "partial" ? "文件操作未成功" : "连接中断"));
        const order = await page.locator(".msg--assistant").last().evaluate(element => {
          const answer = element.querySelector(".markdown-body"), notice = element.querySelector(".execution-notice");
          return Boolean(answer && notice && (answer.compareDocumentPosition(notice) & Node.DOCUMENT_POSITION_FOLLOWING));
        });
        assert(order, "the retained answer precedes its result notice");
      }
      const geometry = await process.evaluate(element => {
        const box = element.getBoundingClientRect(), marker = getComputedStyle(element.querySelector(".execution-process__status"));
        const probe = document.createElement("span"); probe.style.color = "var(--echo-text-tertiary)"; document.body.append(probe);
        const neutral = getComputedStyle(probe).color; probe.remove();
        return { x: box.x, right: box.right, overflow: document.documentElement.scrollWidth > innerWidth + 1, clipped: element.scrollWidth > element.clientWidth + 1, color: marker.backgroundColor, neutral };
      });
      assert(!geometry.overflow && !geometry.clipped && geometry.x >= 0 && geometry.right <= width + 1);
      assert.notEqual(geometry.color, geometry.neutral, "confirmed completion and unresolved results have distinct markers");
      await page.screenshot({ path: join(output, `${scenario}-${width}-${theme}.png`) });
      await header.click();
      assert.equal(await process.locator(".toolcall").count(), 2);
      if (scenario === "recovered") await process.getByText("该次失败已通过后续重试恢复", { exact: true }).waitFor();
      await process.locator(".toolcall--err").first().click();
      await page.getByText(scenario === "partial" ? "报告目录无写入权限" : "首次连接超时", { exact: true }).waitFor();
      await page.getByRole("button", { name: "关闭面板", exact: true }).click();
      if (width === 1440) {
        await page.locator('.sidebar__conv[data-session-id="review-other"] .sidebar__conv-select').click();
        await page.locator('.sidebar__conv[data-session-id="review-conversation"] .sidebar__conv-select').click();
        await page.locator(".execution-process__body").waitFor();
      }
      if (scenario === "partial") {
        const before = await page.evaluate(() => window.workbenchCalls.filter(call => call.command === "review_send").length);
        await page.getByRole("button", { name: "检查并继续", exact: true }).click();
        const call = await page.evaluate(() => window.workbenchCalls.filter(call => call.command === "review_send").at(-1));
        assert(call.args.text.includes("不要重复执行已完成的操作"));
        assert.equal(await page.evaluate(() => window.workbenchCalls.filter(call => call.command === "review_send").length), before + 1);
        assert.equal(await page.locator(".msg--user").count(), 2, "continuation must not rewind or delete the original turn");
      }
      evidence.push({ scenario, width, height, theme, geometry });
    }
  }
  for (const scenario of ["all-failed", "unfinished", "history", "uncertain-send", "browser-upload", "uncertain-browser-upload"]) {
    await page.setViewportSize({ width: 1280, height: 800 }); await page.goto(`${base}&theme=light`);
    await page.locator(".session-header").waitFor(); await setup(scenario); await settle(scenario);
    await page.getByRole("button", { name: /执行记录/ }).waitFor();
    assert.equal(await page.locator(".session-header [role=status]").count(), 0);
    assert.equal(await page.locator(".execution-notice").count(), scenario === "history" ? 0 : 1);
    if (["unfinished", "uncertain-send", "uncertain-browser-upload"].includes(scenario)) assert((await page.locator(".execution-notice").textContent()).includes("结果尚未确认"));
  }
  assert.deepEqual(errors, []);
  writeFileSync(join(output, "evidence.json"), JSON.stringify({ layouts: evidence, errors }, null, 2));
  console.log(JSON.stringify({ ok: true, output, layouts: evidence.length, checks: ["explicit process outcome", "alternate recovery", "single runtime error", "delivery impact", "uncertain side effect", "history", "retained records", "safe continuation", "session restoration"], errors }, null, 2));
} catch (error) {
  if (page) {
    await page.screenshot({path: join(output, "failure.png")}).catch(() => {});
    writeFileSync(join(output, "failure-text.txt"), await page.locator("body").innerText().catch(() => "unavailable"));
    console.error("Page errors:", await page.evaluate(() => ({titles: [...document.querySelectorAll(".execution-process__title")].map(e => e.textContent), processes: document.querySelectorAll(".execution-process").length})).catch(() => null));
  }
  throw error;
} finally { await browser?.close(); await server.close(); }
