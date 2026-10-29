import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createUiReviewServer, reviewChromium } from "./ui-review-runtime.mjs";

const output = mkdtempSync(join(tmpdir(), "echo-content-menus-"));
const server = await createUiReviewServer(0);
const evidence = [];
let browser;
const geometry = locator => locator.evaluate(element => {
  const r = element.getBoundingClientRect();
  return { left: r.left, right: r.right, width: r.width, top: r.top, bottom: r.bottom, scrollWidth: element.scrollWidth, clientWidth: element.clientWidth };
});
const fits = (r, width) => r.left >= 0 && r.right <= width + 1 && r.top >= 0 && r.bottom <= 800 && r.scrollWidth <= r.clientWidth + 1;
try {
  browser = await (await reviewChromium()).launch({ headless: true, executablePath: process.env.CHROME_BIN || "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" });
  for (const width of [1280, 768, 350]) for (const theme of ["light", "dark"]) for (const font of [16, 22]) {
    const page = await browser.newPage({ viewport: { width, height: 800 } });
    page.setDefaultNavigationTimeout(60000);
    const errors = [];
    page.on("pageerror", error => errors.push(error.message));
    await page.goto(`http://127.0.0.1:${server.httpServer.address().port}/__ui-review?surface=content-menus&theme=${theme}`);
    await page.getByRole("button", { name: "操作菜单示例", exact: true }).waitFor();
    await page.evaluate(font => { document.documentElement.style.fontSize = `${font}px`; }, font);
    await page.getByRole("button", { name: "操作菜单示例", exact: true }).click();
    const menu = page.getByRole("menu", { name: "操作菜单示例" });
    const compact = await geometry(menu);
    assert.ok(fits(compact, width) && compact.width <= 210, "simple actions should not reserve a fixed wide column");
    await menu.getByRole("menuitem", { name: "查看导出格式", exact: true }).click();
    const expanded = await geometry(menu);
    assert.ok(fits(expanded, width) && expanded.width > compact.width + 30, "explanatory content should receive reading space");
    await page.keyboard.press("ArrowLeft");
    await menu.getByRole("menuitem", { name: "重命名", exact: true }).waitFor();
    const restored = await geometry(menu);
    assert.ok(Math.abs(restored.width - compact.width) < 1, "returning from an expanded view must release its width");
    assert.ok(fits(restored, width), "a resized menu must stay anchored inside the window");
    await menu.screenshot({ path: join(output, `actions-${width}-${theme}-${font}.png`) });
    await page.keyboard.press("Escape");
    await page.getByRole("button", { name: /^示例模型/ }).click();
    const models = page.getByRole("listbox", { name: "示例模型" });
    const modelBox = await geometry(models);
    assert.ok(fits(modelBox, width) && modelBox.width <= 320, "long model names must not widen a menu to the window");
    const labelFits = await models.locator(".model-selector__item-label, .model-selector__connection").evaluateAll(elements => elements.every(element => element.scrollWidth <= element.clientWidth + 1));
    assert.ok(labelFits, "long model labels must wrap without being clipped");
    await models.screenshot({ path: join(output, `models-${width}-${theme}-${font}.png`) });
    await models.getByRole("option", { name: /常用模型/ }).click();
    assert.equal(await page.getByTestId("selected-model").textContent(), "review/short-model");
    await page.getByRole("button", { name: /^工作目录：/ }).click();
    const directories = page.getByRole("menu", { name: "选择工作目录" });
    const directoryBox = await geometry(directories);
    assert.ok(fits(directoryBox, width) && directoryBox.width <= 320, "directory menus must honor their content cap");
    await page.keyboard.press("Escape");
    assert.deepEqual(errors, []);
    evidence.push({ width, theme, font, compact, expanded, restored, modelBox, directoryBox });
    await page.close();
  }
  writeFileSync(join(output, "geometry.json"), JSON.stringify(evidence, null, 2));
  console.log(`Content menus passed: ${evidence.length} layouts; intrinsic action widths, expanding and shrinking views, long model/path caps and original model selection. Screenshots: ${output}`);
} finally {
  await browser?.close();
  await server.close();
}
