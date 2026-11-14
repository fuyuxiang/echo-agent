import assert from "node:assert/strict";
import { join } from "node:path";

/** Exercise receipt updates and reading intent in the production ChatView. */
export async function validateExecutionProgressInteractions(page, base, output) {
  const reset = async () => {
    await page.goto(`${base}/__ui-review?surface=execution&state=running&theme=light`);
    await page.locator('[data-execution-ready="true"]').waitFor();
  };
  const tool = (id, status = "completed", kind = "bash") => ({
    kind: "tool_call", toolCall: { toolCallId: id, kind, title: id, status, content: [] },
  });
  const set = async (parts, complete = false) => {
    await page.evaluate(async ({ parts, complete }) => {
      const { useSessionStore } = await import("/src/stores/session-store.ts");
      const store = useSessionStore.getState();
      const id = store.streamingMessageId ?? [...store.messages].reverse().find(message => message.role === "assistant")?.id;
      const promptId = store.messages.find(message => message.id === id)?.promptId;
      // Complete through the actual ACK rail below, after installing an open
      // message. Pre-closing it would make the ACK look like a replay duplicate.
      store.setMessages([{ id, role: "assistant", complete: false, promptId,
        startedAt: Date.now() - 5000, lastActivityAt: Date.now(), stopReason: complete ? "end_turn" : undefined, parts }]);
      if (complete) store.markComplete({ sessionId: store.sessionId, promptId: promptId ?? "", stopReason: "end_turn" });
    }, { parts, complete });
    await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
  };
  const later = ["one", "two", "three"].map(id => tool(id));

  await reset();
  await set([tool("slow-a", "in_progress"), tool("slow-b", "in_progress"), ...later]);
  assert.equal(await page.locator(".execution-process__live .toolcall--run").count(), 1,
    "parallel work has one featured operation and a visible summary");
  await page.getByRole("button", { name: "另有 1 项操作进行中", exact: true }).click();
  assert.equal(await page.locator(".execution-process__body .toolcall--run").count(), 2);

  await reset();
  const reads = [tool("read-a", "completed", "read_file"), tool("read-b", "completed", "read_file")];
  await set(reads);
  const group = await page.locator(".execution-process__activity-group").elementHandle();
  await group.focus();
  await set([...reads, tool("read-c", "completed", "read_file"), ...later]);
  assert(await group.evaluate(el => el === document.activeElement), "a focused group must retain its DOM node");
  assert.match(await group.textContent(), /文件读取 · 2 次操作/);
  const corrected = reads.map(part => part.toolCall.toolCallId === "read-b"
    ? { ...part, toolCall: { ...part.toolCall, status: "failed" } } : part);
  await set([...corrected, ...later]);
  assert(await group.evaluate(el => el === document.activeElement));
  assert.match(await group.textContent(), /1 项失败/, "retaining a group must still display updated receipts");
  await set([...corrected, ...later], true);
  assert(await group.evaluate(el => el === document.activeElement));
  await group.evaluate(el => el.blur());
  await page.locator(".execution-process__live").waitFor({ state: "detached" });

  for (const grouped of [false, true]) {
    await reset();
    const parts = grouped ? reads : [tool("selected")];
    await set(parts);
    const label = page.locator(grouped ? ".execution-process__activity-group > span" : ".toolcall__title").first();
    const selected = await label.textContent();
    await label.evaluate(el => {
      const range = document.createRange(); range.selectNodeContents(el);
      const selection = document.getSelection(); selection.removeAllRanges(); selection.addRange(range);
      document.dispatchEvent(new Event("selectionchange"));
    });
    await set([...parts, ...later]);
    assert.equal(await page.evaluate(() => document.getSelection().toString()), selected);
    await set([...parts, ...later, { kind: "text", phase: "final_answer", text: "已整理结果。" }]);
    assert.equal(await page.evaluate(() => document.getSelection().toString()), selected);
    await page.evaluate(() => {
      document.getSelection().removeAllRanges();
      document.dispatchEvent(new Event("selectionchange"));
    });
    await page.locator(".execution-process__live").waitFor({ state: "detached" });
  }

  await reset();
  await set([tool("waiting-focus", "in_progress")]);
  const focused = await page.locator('[data-tool-call-id="waiting-focus"]').elementHandle();
  await focused.focus();
  for (const kind of ["question", "permission"]) {
    await page.evaluate(async kind => {
      const { useSessionStore } = await import("/src/stores/session-store.ts");
      const { sessionId } = useSessionStore.getState();
      if (kind === "question") {
        const { useQuestionStore } = await import("/src/stores/question-store.ts");
        useQuestionStore.getState().request({ requestId: "progress-question", sessionId, toolCallId: "waiting-focus", title: "确认范围", mode: "default",
          questions: [{ id: "scope", question: "需要检查哪些资料？", options: [], multiSelect: false }] });
      } else {
        const { usePermissionStore } = await import("/src/stores/permission-store.ts");
        usePermissionStore.getState().request({ requestId: "progress-permission", sessionId, toolCallId: "waiting-focus", toolKind: "read_file", title: "读取资料",
          options: [{ optionId: "allow", title: "允许一次", kind: "allow_once" }] });
      }
    }, kind);
    await page.locator(".execution-process__title").getByText(kind === "question" ? "等待你补充信息" : "等待你批准操作", { exact: true }).waitFor();
    assert.equal(await page.locator(".execution-process__header").getAttribute("aria-expanded"), "false");
    assert(await focused.evaluate(el => el === document.activeElement), "waiting must not replace the focused live action");
    await page.evaluate(async kind => {
      const { useSessionStore } = await import("/src/stores/session-store.ts");
      const { sessionId } = useSessionStore.getState();
      if (kind === "question") {
        const { useQuestionStore } = await import("/src/stores/question-store.ts");
        useQuestionStore.getState().dismiss("progress-question", sessionId);
      } else {
        const { usePermissionStore } = await import("/src/stores/permission-store.ts");
        usePermissionStore.getState().dismiss("progress-permission", sessionId);
      }
    }, kind);
  }

  await reset();
  const command = "pnpm test -- --project ui";
  await set([{ kind: "tool_call", toolCall: { ...tool("command").toolCall, title: "Run test", rawInput: { command } } }]);
  const commandButton = page.getByRole("button", { name: `bash ${command} 完成`, exact: true });
  assert.match(await commandButton.getAttribute("title"), /pnpm test -- --project ui/);
  assert.match(await commandButton.textContent(), /pnpm test -- --project ui/);

  await reset();
  const unknown = { kind: "tool_call", toolCall: { toolCallId: "unknown", kind: "other", toolName: "computer_click", title: "点击按钮", status: "failed",
    content: [{ type: "text", text: '{"status":"result_unknown","receiptId":"action-1"}' }] } };
  await set([unknown, ...later]);
  assert.match(await page.locator(".execution-process__attention").textContent(), /1 项操作结果待确认/);
  await page.locator(".execution-process__header").click();
  assert.equal(await page.locator(".execution-process__metrics").textContent(), "已完成 3 项操作 · 1 项结果待确认");
  assert.equal(await page.locator(".toolcall--err").count(), 0);
  await page.locator(".execution-process__header").click();
  await set([unknown, ...later, { kind: "text", phase: "final_answer", text: "请核验点击结果。" }], true);
  assert.match(await page.locator(".execution-notice").textContent(), /电脑操作的结果尚未确认/);
  await page.screenshot({ path: join(output, "pending-desktop-outcome.png") });
  await set([unknown, ...later, { kind: "tool_call", toolCall: { toolCallId: "verified", kind: "other", toolName: "automation_complete", title: "核验成果", status: "completed",
    content: [{ type: "text", text: '{"task":{"phase":"completed","receipts":[{"id":"action-1"}]}}' }] } }], true);
  await page.locator(".execution-notice").waitFor({ state: "detached" });
  await page.locator(".execution-process__header").click();
  await page.getByRole("button", { name: /点击按钮 后续任务已核验$/ }).click();
  assert.equal(await page.locator(".tool-detail__status").textContent(), "后续任务已核验");

  for (const [context, query] of [["ordinary", ""], ["project", "&project=1"],
    ["browser", "&automationMode=browser_use"], ["desktop", "&automationMode=computer_use"]]) {
    await page.goto(`${base}/__ui-review?surface=execution&state=running&theme=light${query}`);
    await page.locator('[data-execution-ready="true"]').waitFor();
    const analysis = { kind: "thought", text: "核对来源并保留依据。", streamId: "unified" };
    await set([analysis]);
    const header = page.locator(".execution-process__header");
    assert.equal(await header.getAttribute("aria-expanded"), "false");
    await header.click();
    await page.getByText(analysis.text, { exact: true }).waitFor();
    assert.equal(await page.locator(".execution-process__stage-heading").count(), 0);
    assert.equal(await page.getByText("分析详情", { exact: true }).count(), 0);
    if (context === "project") await page.getByRole("button", { name: "资料核对项目", exact: true }).waitFor();
    await page.screenshot({ path: join(output, `unified-${context}-analysis.png`) });
    await header.click();
    if (context === "browser" || context === "desktop") {
      const native = { kind: "tool_call", toolCall: { toolCallId: "native-between-turns", kind: "other",
        toolName: context === "browser" ? "browser_snapshot" : "computer_screenshot", title: "读取应用界面", status: "in_progress", content: [] } };
      await set([tool("model-ended", "in_progress", "read_file"), native], true);
      await page.locator(".execution-process--running").waitFor();
      assert.equal(await page.locator(".execution-notice").count(), 0, "native work continues without a false empty-answer notice");
      await header.click();
      await page.getByLabel("应用操作情况", { exact: true }).waitFor();
      assert.doesNotMatch(await page.locator(".execution-process").innerText(), /原生控件|鼠标键盘后备|general-purpose/);
      const nativeRow = page.locator('.execution-process [data-tool-call-id="native-between-turns"]');
      const endedRow = page.locator('.execution-process [data-tool-call-id="model-ended"]');
      assert.match(await nativeRow.innerText(), /运行中/);
      assert.match(await endedRow.innerText(), /结果待确认/);
      await nativeRow.click();
      assert.equal(await page.locator(".tool-detail__status").innerText(), "运行中", "native record and detail agree between model turns");
      await page.getByRole("button", { name: "关闭面板", exact: true }).click();
      await endedRow.click();
      assert.equal(await page.locator(".tool-detail__status").innerText(), "结果待确认", "native activity cannot revive the model's old file operation");

      await page.goto(`${base}/__ui-review?surface=execution&state=running&theme=light${query}`);
      await page.locator('[data-execution-ready="true"]').waitFor();
      const ongoing = tool("model-active", "in_progress", "read_file");
      await set([native, ongoing]);
      await page.evaluate(() => {
        const invoke = window.__TAURI_INTERNALS__.invoke;
        window.__TAURI_INTERNALS__.invoke = async (command, args) => {
          const result = await invoke(command, args);
          return command === "automation_status" ? { ...result, paused: true,
            task: { ...result.task, phase: "paused", authorized: false } } : result;
        };
        window.dispatchEvent(new Event("focus"));
      });
      await page.locator(".execution-process__native-status").waitFor();
      assert.match(await page.locator(".execution-process__native-status").innerText(), /应用操作已中断，本轮其他处理仍在继续/);
      assert.equal(await page.locator(".execution-process--running").count(), 1);
      assert.equal(await page.locator(".execution-process__live .toolcall--run").count(), 1);
      await page.locator(".execution-process__header").click();
      await nativeRow.click();
      assert.equal(await page.locator(".tool-detail__status").innerText(), "结果待确认");
      await page.getByRole("button", { name: "关闭面板", exact: true }).click();
      await page.locator('.execution-process [data-tool-call-id="model-active"]').click();
      assert.equal(await page.locator(".tool-detail__status").innerText(), "运行中", "native pause leaves file work live in records and details");
      await set([native, ongoing], true);
      await page.locator(".execution-process--stopped").waitFor();
      assert.equal(await page.locator(".execution-process .toolcall--run").count(), 0);
      assert.equal(await page.locator(".tool-detail__status").innerText(), "结果待确认");
      await page.screenshot({ path: join(output, `unified-${context}-ownership.png`) });
    }
  }

  await reset();
  const planned = { ...tool("single-stage"), stageTitle: "核对唯一计划阶段" };
  await set([planned], true);
  await page.locator(".execution-process__header").click();
  await page.getByRole("heading", { name: "核对唯一计划阶段", exact: true }).waitFor();
  await page.screenshot({ path: join(output, "completed-plan-heading.png") });

  await reset();
  const sameName = ["frontend", "backend"].map(directory => ({ kind: "tool_call", toolCall: {
    toolCallId: directory, kind: "read_file", title: `Read /review/isolated-EchoAgent/${directory}/config.ts`, status: "completed", content: [],
    rawInput: { path: `/review/isolated-EchoAgent/${directory}/config.ts` },
  } }));
  await set(sameName, true);
  await page.locator(".execution-process__header").click();
  for (const directory of ["frontend", "backend"]) {
    const row = page.getByRole("button", { name: `read_file ${directory}/config.ts 完成`, exact: true });
    await row.waitFor();
    assert.match(await row.getAttribute("title"), new RegExp(`${directory}/config.ts`));
  }
  await page.screenshot({ path: join(output, "full-record-file-paths.png") });
  await page.goto(`${base}/__ui-review?surface=execution&state=error&theme=light`);
  await page.locator(".execution-notice").waitFor();
  assert.equal(await page.locator(".chatview__error-banner").count(), 0, "owned runtime errors have one recovery surface");
  assert.equal(await page.getByText("本轮未能继续", { exact: true }).count(), 1);
}
