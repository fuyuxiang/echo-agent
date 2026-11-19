import { openWorkspaceView } from "./session-review-navigation.mjs";
import assert from "node:assert/strict";
import { join } from "node:path";

// Real layout checks: jsdom cannot detect a centered composer moving without
// resizing, overlapping text, or native scroll events after a panel unmounts.
export async function validateConversationLayout(page, output) {
  let layouts = 0;
  const atBottom = () => page.waitForFunction(() => {
    const viewport = document.querySelector(".chatview__scroll");
    return viewport && Math.abs(viewport.scrollHeight - viewport.clientHeight - viewport.scrollTop) < 2
      && !document.querySelector(".chatview__jump-latest");
  });
  const summaryAligned = () => page.waitForFunction(() => {
    const summary = document.querySelector(".session-background-tasks")?.getBoundingClientRect();
    const header = document.querySelector(".session-header")?.getBoundingClientRect();
    return summary && header && summary.top >= header.top && summary.bottom <= header.bottom
      && summary.left >= header.left && summary.right <= header.right;
  });
  const rows = () => page.locator(".sidebar__conv").evaluateAll(elements => elements.map(element => {
    const rect = node => {
      const box = node.getBoundingClientRect();
      return { top: box.top, bottom: box.bottom, left: box.left, right: box.right, height: box.height };
    };
    return { row: rect(element), title: rect(element.querySelector(".sidebar__conv-title")),
      metadata: [...element.querySelectorAll(".sidebar__conv-status,.sidebar__conv-pin")].map(rect) };
  }));

  for (const [width, height, theme] of [[1920, 1080, "light"], [1024, 768, "dark"], [768, 720, "light"]]) {
    await page.setViewportSize({ width, height });
    await page.goto(`http://127.0.0.1:1439/__ui-review?surface=conversation-layout&theme=${theme}`);
    await page.getByRole("button", { name: /后台任务/ }).waitFor();
    await summaryAligned();
    const toolbar = page.locator(".session-header");
    const openView = label => openWorkspaceView(page, label);
    const originalComposer = await page.locator(".chatview .echo-composer").boundingBox();
    await openView("网页预览");
    await summaryAligned();
    const movedComposer = await page.locator(".chatview .echo-composer").boundingBox();
    if (width === 1920) {
      assert.ok(Math.abs(originalComposer.width - movedComposer.width) < 1, "wide composer should retain its width");
      assert.ok(originalComposer.x - movedComposer.x > 100, "tool pane must exercise a position-only move");
    }
    await page.screenshot({ path: join(output, `conversation-dock-${width}-${theme}.png`) });
    layouts += 1;
    await openView("网页预览");
    await summaryAligned();
    await page.getByRole("button", { name: "收起侧边栏", exact: true }).click();
    await summaryAligned();
    await page.getByRole("button", { name: "展开侧边栏", exact: true }).click();
    await summaryAligned();

    const viewport = page.locator(".chatview__scroll");
    for (const label of ["变更", "团队"]) {
      await page.locator('.sidebar__conv[data-session-id="review-conversation"] .sidebar__conv-select').click();
      // Returning to A restores its details view as well as its reading position.
      const closeDetails = page.getByRole("button", { name: "关闭面板", exact: true });
      if (await closeDetails.isVisible()) await closeDetails.click();
      const jump = page.getByRole("button", { name: "回到最新消息" });
      if (await jump.isVisible()) await jump.click();
      await atBottom();
      await viewport.evaluate(element => {
        element.scrollTop = 650;
        element.dispatchEvent(new WheelEvent("wheel", { deltaY: -1 }));
        element.dispatchEvent(new Event("scroll"));
      });
      await page.getByRole("button", { name: "回到最新消息" }).waitFor();
      await openView(label === "团队" ? "管理任务团队" : "文件变更");
      await page.getByRole("button",{name:"关闭面板",exact:true}).click();
      await page.waitForFunction(() => Math.abs(document.querySelector(".chatview__scroll").scrollTop - 650) < 2);
      await openView(label === "团队" ? "管理任务团队" : "文件变更");
      await page.locator('.sidebar__conv[data-session-id="review-other"] .sidebar__conv-select').click();
      await atBottom();
      assert.equal(await page.locator(".chatview__runtime-view:visible").count(), 0, "new session must not inherit a runtime panel");
      await openView(label === "团队" ? "管理任务团队" : "文件变更");
      await page.getByRole("button", { name: "关闭面板", exact: true }).click();
      await atBottom();
    }

    for (const fontSize of [13, 18]) {
      await page.evaluate(size => { document.documentElement.style.fontSize = `${16 * size / 13}px`; }, fontSize);
      await summaryAligned();
      const before = await rows();
      for (const row of before) {
        assert.ok(row.title.top >= row.row.top && row.title.bottom <= row.row.bottom, "session title escapes its row");
        for (const metadata of row.metadata) {
          assert.ok(metadata.height > 0 && metadata.left >= row.title.right + 1, "session status overlaps title");
          assert.ok(metadata.bottom <= row.row.bottom && metadata.right <= row.row.right, "session metadata escapes its row");
        }
      }
      await page.locator(".sidebar__conv").first().hover();
      await page.waitForFunction(() => getComputedStyle(document.querySelector(".sidebar__conv-action")).opacity === "1");
      const hovered = (await rows())[0];
      assert.deepEqual(hovered, before[0], "hover must keep session title and metadata positions stationary");
      assert.equal(await page.locator(".sidebar__conv").first().locator(".sidebar__conv-action").evaluate(element => getComputedStyle(element).opacity), "1", "hover must reveal the session action");
      assert.equal(await page.locator(".sidebar__conv").first().locator(".sidebar__conv-status").evaluate(element => getComputedStyle(element).visibility), "visible", "hover must keep the status dot visible beside the action");
      await page.mouse.move(width - 10, 10);
      const first = page.locator(".sidebar__conv").first();
      await first.locator(".sidebar__conv-select").focus();
      assert.equal(await first.locator(".sidebar__conv-status").evaluate(element => getComputedStyle(element).visibility), "visible", "selection focus must keep the status visible");
      await page.keyboard.press("Tab");
      await page.waitForFunction(() => getComputedStyle(document.querySelector(".sidebar__conv-action")).opacity === "1");
      assert.equal(await first.locator(".sidebar__conv-status").evaluate(element => getComputedStyle(element).visibility), "visible", "keyboard action focus must keep the status dot visible");
      await page.evaluate(() => { if (document.activeElement instanceof HTMLElement) document.activeElement.blur(); });
      await page.waitForFunction(() => getComputedStyle(document.querySelector(".sidebar__conv-action")).opacity === "0");
      assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1), "chat layout overflows horizontally");
      await page.screenshot({ path: join(output, `conversation-sidebar-${fontSize}px-${width}-${theme}.png`) });
      layouts += 1;
    }
  }
  return layouts;
}
