import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createUiReviewServer, reviewChromium, reviewOutput } from "./ui-review-runtime.mjs";

const output = reviewOutput(mkdtempSync(join(tmpdir(), "echo-sidebar-toolbar-")));
console.log(`Sidebar toolbar evidence: ${output}`);
const server = await createUiReviewServer(0);
const base = `http://127.0.0.1:${server.httpServer.address().port}/__ui-review`;
const evidence = [];
const catalogEvidence = [];
const macWindow = JSON.parse(readFileSync(new URL("../src-tauri/tauri.macos.conf.json", import.meta.url), "utf8")).app.windows[0];
let browser;

try {
  browser = await (await reviewChromium()).launch({ headless: true, executablePath: process.env.CHROME_BIN || "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" });
  const page = await browser.newPage();
  page.setDefaultNavigationTimeout(60000);
  const errors = [];
  page.on("pageerror", error => errors.push(error.message));
  for (const [width, height, sidebarWidth, font] of [[1280, 800, 248, 16], [1024, 680, 240, 22], [900, 680, 400, 16]]) {
    for (const theme of ["light", "dark"]) for (const platform of ["mac", "other"]) {
      await page.setViewportSize({ width, height });
      await page.goto(`${base}?surface=home&theme=${theme}&platform=${platform}`);
      await page.getByRole("group", { name: "侧边栏工具" }).waitFor();
      await page.locator(".echo-composer").waitFor();
      await page.locator(".sidebar").evaluate((element, width) => element.style.setProperty("--app-sidebar-width", `${width}px`), sidebarWidth);
      await page.evaluate(font => { document.documentElement.style.fontSize = `${font}px`; }, font);
      const name = `${width}-${sidebarWidth}-${font}-${theme}-${platform}`;
      const geometry = await page.evaluate(() => {
        const rect = element => {
          const { x, y, right, bottom, width, height } = element.getBoundingClientRect();
          return { x, y, right, bottom, width, height };
        };
        return {
          sidebar: rect(document.querySelector(".sidebar")),
          toolbar: rect(document.querySelector(".sidebar__toolbar")),
          brand: rect(document.querySelector(".sidebar__logo-row")),
          tools: [...document.querySelectorAll(".sidebar__toolbar-actions button")].map(element => ({ label: element.getAttribute("aria-label"), ...rect(element) })),
          duplicateCollapse: document.querySelectorAll('button[aria-label="收起侧边栏"]').length,
          overflow: document.documentElement.scrollWidth > innerWidth + 1,
        };
      });
      assert.equal(geometry.overflow, false, `${name}: page overflow`);
      assert.equal(geometry.toolbar.height, 56, `${name}: toolbar must leave breathing room`);
      assert.equal(geometry.duplicateCollapse, 1, `${name}: one collapse control`);
      assert.deepEqual(geometry.tools.map(tool => tool.label), ["收起侧边栏", "搜索全部任务", "筛选任务"]);
      for (const [index, tool] of geometry.tools.entries()) {
        assert.ok(tool.width === 32 && tool.height === 32 && tool.right <= geometry.sidebar.right - 16, `${name}: inconsistent tool targets`);
        assert.equal(tool.y - geometry.toolbar.y, 12, `${name}: toolbar top inset`);
        assert.equal(geometry.toolbar.bottom - tool.bottom, 12, `${name}: toolbar bottom inset`);
        assert.ok(tool.bottom <= geometry.brand.y && tool.y === geometry.tools[0].y, `${name}: tools must align above branding`);
        if (index) assert.equal(tool.x - geometry.tools[index - 1].right, 4, `${name}: tool spacing`);
      }
      if (platform === "mac") {
        assert.ok(geometry.tools[0].x >= 84, `${name}: traffic-light safe area`);
        // AppKit's standard button frame centers 2pt below Tao's configured
        // vertical inset; verified against the real Tauri window screenshot.
        assert.equal(geometry.tools[0].y + geometry.tools[0].height / 2, macWindow.trafficLightPosition.y + 2, `${name}: native controls and toolbar must share a center line`);
      }
      await page.mouse.move(width - 10, height - 10);
      await page.screenshot({ path: join(output, `expanded-${name}.png`) });

      // The relocated search opens the same real dialog and restores its trigger.
      const search = page.getByRole("button", { name: "搜索全部任务", exact: true });
      await search.click();
      await page.getByRole("dialog", { name: "搜索会话", exact: true }).waitFor();
      await page.getByRole("textbox", { name: "搜索会话标题或内容", exact: true }).fill("设计");
      await page.keyboard.press("Escape");
      await page.getByRole("dialog", { name: "搜索会话", exact: true }).waitFor({ state: "hidden" });
      assert.ok(await search.evaluate(element => element === document.activeElement), `${name}: search focus return`);

      const filter = page.getByRole("button", { name: "筛选任务", exact: true });
      await filter.focus();
      await filter.press("ArrowDown");
      const menu = page.getByRole("menu", { name: "任务筛选", exact: true });
      await menu.waitFor();
      const bounds = await menu.boundingBox();
      assert.ok(bounds.x >= 0 && bounds.y >= 32 && bounds.x + bounds.width <= width && bounds.y + bounds.height <= height, `${name}: filter menu clipped`);
      assert.ok(await menu.getByRole("menuitemradio").first().evaluate(element => element === document.activeElement), `${name}: filter keyboard entry`);
      await menu.getByRole("menuitemradio", { name: "今天", exact: true }).click();
      assert.equal(await filter.locator(".task-filter-trigger__dot").count(), 1, `${name}: active filter indicator`);
      await page.keyboard.press("Escape");
      assert.ok(await filter.evaluate(element => element === document.activeElement), `${name}: filter focus return`);

      const collapse = page.getByRole("button", { name: "收起侧边栏", exact: true });
      await collapse.focus();
      await collapse.press("Enter");
      const expand = page.getByRole("button", { name: "展开侧边栏", exact: true });
      await expand.waitFor();
      assert.ok(await expand.evaluate(element => element === document.activeElement), `${name}: collapse focus transfer`);
      assert.deepEqual(await page.locator(".main-page-controls button").evaluateAll(buttons => buttons.map(button => button.getAttribute("aria-label"))), ["展开侧边栏", "新建任务"]);
      if (platform === "mac") {
        const expanded = await expand.boundingBox();
        assert.ok(expanded.x >= 84, `${name}: collapsed traffic-light safe area`);
        assert.ok(Math.abs(expanded.y - geometry.tools[0].y) <= 1, `${name}: toolbar must keep its top inset when collapsed`);
      }
      await page.getByRole("button", { name: "新建任务", exact: true }).click();
      assert.ok(await page.evaluate(() => window.workbenchNewTask), `${name}: new task callback`);
      assert.ok(await page.locator(".echo-composer").isVisible(), `${name}: collapsed page content`);
      await page.mouse.move(width - 10, height - 10);
      await page.screenshot({ path: join(output, `collapsed-${name}.png`) });
      await expand.click();
      assert.ok(await collapse.evaluate(element => element === document.activeElement), `${name}: expand focus return`);
      assert.equal(await menu.count(), 0, `${name}: stale filter menu after collapse`);
      evidence.push({ name, ...geometry, menu: bounds });
    }
  }

  // Exercise the largest supported navigation configuration, all three
  // catalogs and the real font preference maximum, including the 32px
  // Windows/Linux title bar that reduces the available sidebar height.
  const catalogPage = await browser.newPage();
  catalogPage.setDefaultNavigationTimeout(60000);
  catalogPage.on("pageerror", error => errors.push(error.message));
  await catalogPage.addInitScript(() => localStorage.setItem("echoagent.sidebar-menu.v1", JSON.stringify({
    primary: ["projects", "organization", "coding", "capabilities", "knowledge", "automation"], more: [],
  })));
  for (const height of [600, 680, 760, 800]) for (const font of [16, 16 * 18 / 13]) {
    for (const theme of ["light", "dark"]) for (const platform of ["mac", "other"]) {
      await catalogPage.setViewportSize({ width: 1024, height });
      await catalogPage.goto(`${base}?surface=sidebar-scroll&variant=both&groups=12&theme=${theme}&platform=${platform}`);
      await catalogPage.getByRole("region", { name: "最近专家团" }).waitFor();
      await catalogPage.evaluate(font => { document.documentElement.style.fontSize = `${font}px`; }, font);
      const name = `catalog-${height}-${font.toFixed(2)}-${theme}-${platform}`;
      const geometry = await catalogPage.evaluate(() => {
        const rect = element => {
          const { x, y, right, bottom, width, height } = element.getBoundingClientRect();
          return { x, y, right, bottom, width, height };
        };
        const content = document.querySelector(".sidebar__content");
        return {
          content: { ...rect(content), clientHeight: content.clientHeight, scrollHeight: content.scrollHeight },
          footer: rect(document.querySelector(".sidebar__footer")),
          overflow: document.documentElement.scrollWidth > innerWidth + 1,
          lists: [...content.querySelectorAll(".sidebar__group")].map(list => ({
            id: list.id, ...rect(list), clientHeight: list.clientHeight, scrollHeight: list.scrollHeight,
            firstRow: rect(list.querySelector(".sidebar__conv, .sidebar__node, .group-sidebar__row")),
            tallestRow: Math.max(...[...list.querySelectorAll(".sidebar__conv, .sidebar__node, .group-sidebar__row")].map(element => element.getBoundingClientRect().height)),
          })),
        };
      });
      const context = `${name}: ${JSON.stringify(geometry)}`;
      assert.equal(geometry.overflow, false, `catalog horizontal overflow: ${context}`);
      assert.equal(geometry.lists.length, 3);
      assert.ok(geometry.content.bottom <= geometry.footer.y + 1, `catalog overlaps account controls: ${context}`);
      for (const list of geometry.lists) {
        assert.ok(list.height >= list.tallestRow - 1, `catalog must retain even a wrapped row: ${context}`);
        if (height >= 680) assert.ok(list.firstRow.y >= geometry.content.y && list.firstRow.bottom <= geometry.content.bottom + 1, `supported window must show a full row in every catalog: ${context}`);
        const locator = catalogPage.locator(`#${list.id}`);
        const scroll = await locator.evaluate(element => { element.scrollTop = element.scrollHeight; return element.scrollTop; });
        assert.ok(scroll > 0, `catalog must remain independently scrollable: ${context}`);
        await locator.evaluate(element => { element.scrollTop = 0; });
      }
      await catalogPage.getByRole("button", { name: "筛选任务", exact: true }).click();
      const menu = catalogPage.getByRole("menu", { name: "任务筛选", exact: true });
      assert.ok(await menu.evaluate(element => {
        const box = element.getBoundingClientRect();
        return element.contains(document.elementFromPoint(box.x + box.width / 2, box.y + 12));
      }), `catalog scrolling must not clip the toolbar filter: ${context}`);
      await catalogPage.keyboard.press("Escape");
      // At heights below the native minimum the outer catalog may scroll;
      // moving focus to its final item must still reveal the item fully.
      await catalogPage.locator("#sidebar-group-list .group-sidebar__item").last().focus();
      assert.ok(await catalogPage.locator("#sidebar-group-list .group-sidebar__item").last().evaluate(element => {
        const box = element.getBoundingClientRect();
        const content = element.closest(".sidebar__content").getBoundingClientRect();
        return box.y >= content.y - 1 && box.bottom <= content.bottom + 1;
      }), `final catalog item must be reachable by keyboard: ${context}`);
      await catalogPage.locator(".sidebar__content").evaluate(element => { element.scrollTop = 0; });
      await catalogPage.locator("#sidebar-group-list").evaluate(element => { element.scrollTop = 0; });
      await catalogPage.screenshot({ path: join(output, `${name}.png`) });
      catalogEvidence.push({ name, ...geometry });
    }
  }
  assert.deepEqual(errors, [], "sidebar toolbar page errors");
  writeFileSync(join(output, "report.json"), JSON.stringify({ passed: true, evidence, catalogEvidence }, null, 2));
  console.log(`Sidebar toolbar passed: ${evidence.length * 2 + catalogEvidence.length} layouts, search, filters, new task, catalog scrolling and keyboard focus.`);
} finally {
  await browser?.close();
  await server.close();
}
