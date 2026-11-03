import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createUiReviewServer, reviewChromium, reviewOutput } from "./ui-review-runtime.mjs";
import { openWorkspaceView } from "./session-review-navigation.mjs";

// Production sidebar layout, Code bridge and composer; only native IPC and the
// embedded IDE document are fixtures. This does not claim native window/DPI QA.
const output = reviewOutput(mkdtempSync(join(tmpdir(), "echo-product-layout-")));
const server = await createUiReviewServer(0);
const base = `http://127.0.0.1:${server.httpServer.address().port}/__ui-review?surface=coding&codingShell=1`;
let browser, page;
const evidence = [], toolEvidence = [], errors = [];
try {
  browser = await (await reviewChromium()).launch({ headless: true,
    executablePath: process.env.CHROME_BIN || "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" });
  page = await browser.newPage();
  page.setDefaultTimeout(15000);
  page.setDefaultNavigationTimeout(60000);
  page.on("pageerror", error => errors.push(error.message));
  await page.addInitScript(() => {
    localStorage.setItem("app-sidebar-width", "400");
    localStorage.setItem("echo-agent-panel-width", "700");
  });
  for (const [width, height] of [[1024, 680], [1280, 800], [1440, 900], [1920, 1080]]) {
    for (const font of [16, 22]) for (const theme of ["light", "dark"]) {
      await page.setViewportSize({ width, height });
      await page.goto(`${base}&theme=${theme}`);
      await page.getByRole("separator", { name: "调整 Agent 面板宽度" }).waitFor();
      await page.addStyleTag({ content: `html{font-size:${font}px!important}` });
      await page.waitForFunction(() => document.querySelector(".echo-theia-workspace").clientWidth > 0);
      const geometry = await page.evaluate(() => {
        const rect = selector => {
          const element = document.querySelector(selector), box = element.getBoundingClientRect();
          return { left: box.left, top: box.top, right: box.right, bottom: box.bottom,
            width: box.width, height: box.height, overflow: getComputedStyle(element).overflow };
        };
        return { ide: rect(".echo-theia-workspace"), agent: rect(".echo-theia-agent"),
          composer: rect(".echo-theia-agent__composer"), send: rect(".echo-theia-agent__send"),
          sidebar: rect(".sidebar"), preference: localStorage.getItem("echo-agent-panel-width") };
      });
      assert(geometry.ide.width >= 280, `IDE lost usable width: ${JSON.stringify(geometry)}`);
      assert(geometry.agent.left >= geometry.ide.right, "Code panes overlap");
      assert(geometry.agent.right <= width + 1 && geometry.composer.bottom <= height + 1, "Code panel leaves viewport");
      assert(geometry.send.top >= 0 && geometry.send.bottom <= height && geometry.send.right <= width, "send action unreachable");
      assert.equal(geometry.preference, "700", "temporary constraints overwrote user preference");
      evidence.push({ width, height, font, theme, ...geometry });
      await page.screenshot({ path: join(output, `coding-${width}-${font}-${theme}.png`) });
    }
  }
  // Continuous viewport reduction/restoration models content resizing, not a
  // native maximize, physical pixels or an operating-system scaling change.
  for (const width of [1920, 1700, 1440, 1280, 1100, 1024, 1280, 1920]) {
    await page.setViewportSize({ width, height: width === 1024 ? 680 : 900 });
    await page.waitForFunction(() => {
      const root = document.querySelector(".coding-workbench");
      const maximum = Math.max(300, Math.min(800, root.clientWidth - 328));
      return Number(document.querySelector('[aria-label="调整 Agent 面板宽度"]').getAttribute("aria-valuenow")) === Math.min(700, maximum);
    });
  }
  const splitter = page.getByRole("separator", { name: "调整 Agent 面板宽度" });
  const original = Number(await splitter.getAttribute("aria-valuenow"));
  const box = await splitter.boundingBox();
  await page.mouse.move(box.x + box.width / 2, box.y + 100);
  await page.mouse.down();
  await page.mouse.move(box.x - 90, box.y + 110, { steps: 8 });
  assert(Number(await splitter.getAttribute("aria-valuenow")) > original, "drag across IDE iframe was lost");
  await page.screenshot({ path: join(output, "drag-over-iframe.png") });
  await page.keyboard.press("Escape");
  await page.mouse.up();
  assert.equal(Number(await splitter.getAttribute("aria-valuenow")), original, "Escape must restore original width");
  for (const reason of ["blur", "pointercancel"]) {
    const box = await splitter.boundingBox();
    await page.mouse.move(box.x + box.width / 2, box.y + 100);
    await page.mouse.down();
    await page.mouse.move(box.x - 50, box.y + 100);
    await page.evaluate(reason => {
      if (reason === "blur") window.dispatchEvent(new Event("blur"));
      else {
        const handle = document.querySelector('[aria-label="调整 Agent 面板宽度"]');
        // Chromium's primary mouse pointer id is 1.
        handle.dispatchEvent(new PointerEvent("pointercancel", { bubbles: true, pointerId: 1 }));
      }
    }, reason);
    await page.mouse.move(box.x - 100, box.y + 100);
    await page.mouse.up();
    assert.equal(Number(await splitter.getAttribute("aria-valuenow")), original, `${reason} left a drag active`);
  }
  const commitBox = await splitter.boundingBox();
  await page.mouse.move(commitBox.x + commitBox.width / 2, commitBox.y + 100);
  await page.mouse.down();
  await page.mouse.move(commitBox.x + 70, commitBox.y + 100, { steps: 8 });
  await page.mouse.up();
  const committed = await splitter.getAttribute("aria-valuenow");
  assert.equal(await page.evaluate(() => localStorage.getItem("echo-agent-panel-width")), committed);
  await page.mouse.move(commitBox.x - 100, commitBox.y + 100);
  assert.equal(await splitter.getAttribute("aria-valuenow"), committed, "released drag continued moving");
  const leaveBox = await splitter.boundingBox();
  await page.mouse.move(leaveBox.x + leaveBox.width / 2, leaveBox.y + 100);
  await page.mouse.down();
  await page.mouse.move(leaveBox.x + 40, leaveBox.y + 100, { steps: 4 });
  assert.notEqual(await splitter.getAttribute("aria-valuenow"), committed, "leave-page check must start during a drag");
  // Keyboard navigation is possible while a mouse pointer is still held.
  await page.getByRole("button", { name: "新建任务", exact: true }).focus();
  await page.keyboard.press("Enter");
  await page.locator(".app__coding-surface").waitFor({ state: "hidden" });
  await page.mouse.up();
  assert.equal(await page.evaluate(() => localStorage.getItem("echo-agent-panel-width")), committed, "hidden Code committed a cancelled drag");
  await page.getByRole("button", { name: "代码开发", exact: true }).click();
  await splitter.waitFor({ state: "visible" });
  assert.equal(await splitter.getAttribute("aria-valuenow"), committed, "returning to Code lost the saved width");
  assert.equal(await page.locator(".coding-workbench--resizing").count(), 0, "returning Code still owns a stale drag");
  for (const [width, height] of [[1024, 680], [1280, 800], [1920, 1080]]) {
    await page.setViewportSize({ width, height });
    await page.evaluate(() => localStorage.setItem("tool-side-panel-width", "700"));
    await page.goto(`${base.split("?")[0]}?surface=workbench&theme=dark`);
    await openWorkspaceView(page, "文件");
    await page.addStyleTag({ content: "html{font-size:22px!important}" });
    const panel = page.locator(".tool-side-panel");
    await panel.waitFor({ state: "visible" });
    const box = await panel.boundingBox();
    assert(box.x >= 0 && box.y >= 0 && box.x + box.width <= width + 1 && box.y + box.height <= height + 1,
      `tool panel exceeds ${width}×${height}: ${JSON.stringify(box)}`);
    toolEvidence.push({ width, height, font: 22, ...box });
    await page.screenshot({ path: join(output, `tool-panel-${width}-22-dark.png`) });
  }
  const edge = page.getByRole("separator", { name: "调整工作面板宽度" });
  const edgeBox = await edge.boundingBox();
  const panelWidth = await edge.getAttribute("aria-valuenow");
  await page.mouse.move(edgeBox.x + edgeBox.width / 2, edgeBox.y + 120);
  await page.mouse.down();
  await page.mouse.move(edgeBox.x - 40, edgeBox.y + 120, { steps: 8 });
  assert.notEqual(await edge.getAttribute("aria-valuenow"), panelWidth);
  assert.equal(await page.evaluate(() => localStorage.getItem("tool-side-panel-width")), "700", "tool drag preview persisted");
  await page.keyboard.press("Escape");
  await page.mouse.up();
  assert.equal(await edge.getAttribute("aria-valuenow"), panelWidth, "tool Escape did not restore width");
  await edge.focus();
  await page.keyboard.press("ArrowLeft");
  const toolCommitted = await edge.getAttribute("aria-valuenow");
  assert.equal(await page.evaluate(() => localStorage.getItem("tool-side-panel-width")), toolCommitted);
  assert.deepEqual(errors, []);
  writeFileSync(join(output, "evidence.json"), JSON.stringify({ evidence, toolEvidence, flows: ["continuous resize", "iframe drag", "Escape", "blur", "pointercancel", "release", "hide and restore Code during drag", "tool panel preview and Escape", "tool panel keyboard commit"], errors }, null, 2));
  console.log(JSON.stringify({ output, layouts: evidence.length, toolLayouts: toolEvidence.length, flows: 9 }));
} catch (error) {
  if (page && !page.isClosed()) await page.screenshot({ path: join(output, "failure.png") });
  console.error(`Evidence: ${output}`);
  throw error;
} finally { await browser?.close(); await server.close(); }
