import { openWorkspaceView } from "./session-review-navigation.mjs";
import assert from "node:assert/strict";
import { join } from "node:path";

/** Real component regressions for workflows that cross session/view states. */
export async function validateWorkbenchTransitions(page, base, output) {
  const openView = label => openWorkspaceView(page, label);
  const sessionAction = async name => {
    await page.getByRole("button", { name: "任务操作", exact: true }).click();
    await page.getByRole("menuitem", { name, exact: true }).click();
  };
  const jump = page.getByRole("button", { name: "回到最新消息并恢复自动跟随" });
  const atBottom = () => page.waitForFunction(() => {
    const e = document.querySelector(".chatview__scroll");
    return e && Math.abs(e.scrollHeight - e.clientHeight - e.scrollTop) < 2;
  });

  await page.setViewportSize({ width: 1280, height: 900 });
  for (const action of ["rewind", "fork"]) {
    await page.goto(`${base}&state=files`);
    await sessionAction(action === "rewind" ? "回溯…" : "分叉…");
    if (action === "rewind") {
      await page.getByRole("button", { name: /回溯到此处/ }).click();
      const confirm = page.getByRole("alertdialog", { name: "确认回溯（全量）？" });
      assert.ok((await confirm.textContent()).includes("第 1 步执行前"));
      assert.ok((await confirm.textContent()).includes("撤销此步骤及后续"));
      assert.ok((await confirm.textContent()).includes("评审投影仪"));
    }
    await page.evaluate(async () => {
      const { useSessionStore } = await import("/src/stores/session-store.ts");
      useSessionStore.getState().startStreaming("review-conversation", "history-race");
    });
    await page.getByRole("button", { name: action === "rewind" ? "确认回溯" : "创建分叉", exact: true }).waitFor({ state: "hidden" });
    assert.deepEqual(await page.evaluate(() => window.workbenchCalls.filter(c => ["rewind_execute", "session_fork"].includes(c.command))), []);
    await page.evaluate(async () => {
      const { useSessionStore } = await import("/src/stores/session-store.ts");
      useSessionStore.getState().markComplete({ sessionId: "review-conversation", promptId: "history-race", stopReason: "end_turn" });
    });
    await page.getByRole("button", { name: "停止生成", exact: true }).waitFor({ state: "hidden" });
    assert.equal(await page.getByRole("button", { name: "确认回溯", exact: true }).count(), 0);
    assert.equal(await page.getByRole("button", { name: "创建分叉", exact: true }).count(), 0);
    await sessionAction(action === "rewind" ? "回溯…" : "分叉…");
    await page.keyboard.press("Escape");
    await page.getByRole("button", { name: "任务操作", exact: true }).waitFor();
  }
  console.log("History confirmations invalidate when a new turn starts.");

  await page.goto(`${base}&state=empty`);
  await openView("^文件\\s*工作目录$");
  await page.getByRole("complementary", { name: "工作区面板" }).waitFor();
  await openView("网页预览");
  const address = page.getByRole("textbox", { name: "预览网址", exact: true });
  await address.waitFor();
  assert.ok((await address.boundingBox()).width >= 100, "browser address must have usable width in the default panel");
  await page.route("https://example.com/workbench-review", route => route.fulfill({ contentType: "text/html", body: "<!doctype html><title>Preview fixture</title><p>Isolated preview</p>" }));
  await address.fill("https://example.com/workbench-review");
  await page.getByRole("button", { name: "预览", exact: true }).click();
  await page.locator(".browser-preview__frame").waitFor();
  assert.equal(await page.locator(".browser-preview__frame").getAttribute("src"), "https://example.com/workbench-review");
  await openView("文件");
  await page.locator(".task-details__list").waitFor();
  await openView("网页预览");
  assert.equal(await page.locator(".browser-preview__frame").getAttribute("src"), "https://example.com/workbench-review", "view changes preserve the browser tab");
  await page.screenshot({ path: join(output, "empty-session-workspace.png") });
  console.log("Empty sessions retain directory and web browsing.");

  for (const following of [true, false]) {
    await page.goto(`${base}&state=files`);
    await atBottom();
    if (!following) {
      await page.locator(".chatview__scroll").evaluate(e => {
        e.scrollTop = 600;
        e.dispatchEvent(new WheelEvent("wheel", {deltaY:-1}));
        e.dispatchEvent(new Event("scroll"));
      });
      await jump.waitFor();
    }
    const anchor = await page.locator(".chatview__scroll").evaluate(e => {
      const top=e.getBoundingClientRect().top;
      const row=[...e.querySelectorAll("[data-msg-id]")].find(row=>row.getBoundingClientRect().bottom>top);
      return {id:row?.dataset.msgId, offset:row ? row.getBoundingClientRect().top-top : 0};
    });
    const assertAnchor = async () => {
      if (following) { await atBottom(); return; }
      await page.waitForFunction(anchor => {
        const viewport=document.querySelector(".chatview__scroll");
        const row=[...viewport.querySelectorAll("[data-msg-id]")].find(row=>row.dataset.msgId===anchor.id);
        return row && Math.abs(row.getBoundingClientRect().top-viewport.getBoundingClientRect().top-anchor.offset)<3;
      }, anchor);
      assert.ok(await jump.isVisible(), "view changes must not resume following");
    };
    await openView("文件变更"); await assertAnchor();
    assert.equal(await page.locator(".chatview__inner .chatview__runtime-view").count(),0);
    await page.getByRole("button",{name:"关闭面板",exact:true}).click(); await assertAnchor();
    await openView("网页预览"); await assertAnchor();
    await page.getByRole("button",{name:"关闭面板",exact:true}).click(); await assertAnchor();
  }
  await openView("任务产物");
  await page.getByRole("complementary", {name:"工作区面板"}).getByRole("button", { name: /^report\.md/ }).click();
  const reportTab = page.getByRole("tab", { name: /report\.md/ });
  await page.getByRole("button",{name:"关闭标签 report.md",exact:true}).waitFor();
  await openView("网页预览");
  await address.fill("https://example.com/workbench-review");
  await page.getByRole("button",{name:"预览",exact:true}).click();
  await reportTab.click();
  await page.getByRole("tab", { name: "成果", exact: true }).waitFor();
  await page.getByRole("button", { name: "查看关联工具", exact: true }).waitFor();
  await page.screenshot({path:join(output,"artifact-preview.png")});
  console.log("Message tool navigation preserves fresh reading snapshots.");

  for (const width of [900, 1280]) for (const theme of ["light", "dark"]) for (const mode of ["browser", "computer"]) {
    await page.setViewportSize({ width, height: 720 });
    await page.goto(`${base}&state=${mode}-paused&theme=${theme}`);
    const feedback = page.locator(".chatview__scroll .session-turn-feedback");
    await feedback.getByRole("button", { name: "继续任务", exact: true }).waitFor();
    assert.equal(await page.locator(".task-execution-status").count(), 0, "no mode-specific toolbar");
    assert.equal(await page.locator(".chatview__footer .yield-banner").count(), 0, "task feedback belongs to the turn");
    assert.equal(await page.locator(".tasks-panel").count(), 0, "no floating background controls");
    assert.equal(await page.getByText("浏览器数据与访问范围", {exact:true}).count(), 0);
    await feedback.getByRole("button", { name: "继续任务", exact: true }).click({ trial: true });
    await page.screenshot({ path: join(output, `unified-pause-${mode}-${width}-${theme}.png`) });
  }
  console.log("All 8 browser/computer layouts use ordinary turn feedback with no dedicated dock.");

  for (const width of [900, 1536]) for (const theme of ["light", "dark"]) {
    const url = new URL(base);
    url.searchParams.set("surface", "coding");
    url.searchParams.set("codingShell", "1");
    url.searchParams.set("theme", theme);
    await page.setViewportSize({ width, height: 900 });
    await page.goto(url.href);
    const toggle = page.getByRole("button", { name: "收起侧边栏", exact: true });
    await toggle.click();
    assert.ok(await page.locator(".app__body").evaluate(e => e.classList.contains("app__body--collapsed")));
    const expand = page.getByRole("button", { name: "展开侧边栏", exact: true });
    assert.ok(await expand.evaluate(e => e === document.activeElement));
    await page.keyboard.press("Enter");
    assert.ok(await toggle.evaluate(e => e === document.activeElement));
    assert.equal(await toggle.getAttribute("aria-expanded"), "true");
    assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1));
    await page.screenshot({ path: join(output, `coding-sidebar-${width}-${theme}.png`) });
  }
  console.log("Workbench transitions passed: live history invalidation, rewind boundary, empty-session resources, fresh reading snapshots, 8 unified tool-mode layouts and 4 coding sidebar layouts.");
}
