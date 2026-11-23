import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createUiReviewServer, reviewChromium, reviewOutput } from "./ui-review-runtime.mjs";
import { openWorkspaceView } from "./session-review-navigation.mjs";

// Exercise the shared workbench side panel with isolated task data.
const output = reviewOutput(mkdtempSync(join(tmpdir(), "echo-product-layout-")));
const server = await createUiReviewServer(0);
const base = `http://127.0.0.1:${server.httpServer.address().port}/__ui-review`;
let browser, page;
const toolEvidence = [], errors = [];
try {
  browser = await (await reviewChromium()).launch({ headless: true,
    executablePath: process.env.CHROME_BIN || "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" });
  page = await browser.newPage();
  page.setDefaultTimeout(15000);
  page.setDefaultNavigationTimeout(60000);
  page.on("pageerror", error => errors.push(error.message));
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
  writeFileSync(join(output, "evidence.json"), JSON.stringify({ toolEvidence, flows: [ "tool panel preview and Escape", "tool panel keyboard commit"], errors }, null, 2));
  console.log(JSON.stringify({ output, toolLayouts: toolEvidence.length, flows: 2 }));
} catch (error) {
  if (page && !page.isClosed()) await page.screenshot({ path: join(output, "failure.png") });
  console.error(`Evidence: ${output}`);
  throw error;
} finally { await browser?.close(); await server.close(); }
