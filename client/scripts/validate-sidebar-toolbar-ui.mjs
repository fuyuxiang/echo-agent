import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createUiReviewServer, reviewChromium, reviewOutput } from "./ui-review-runtime.mjs";
import { assertPrimaryAction } from "./ui-primary-action-audit.mjs";
import { inspectRenderedColors } from "./ui-color-audit.mjs";

const output = reviewOutput(mkdtempSync(join(tmpdir(), "echo-sidebar-toolbar-")));
console.log(`Sidebar toolbar evidence: ${output}`);
const server = await createUiReviewServer(0);
const base = `http://127.0.0.1:${server.httpServer.address().port}/__ui-review`;
const evidence = [];
const catalogEvidence = [];
const hierarchyEvidence = [];
const macWindow = JSON.parse(readFileSync(new URL("../src-tauri/tauri.macos.conf.json", import.meta.url), "utf8")).app.windows[0];
let browser;

async function emulatePlatform(page) {
  await page.addInitScript(() => {
    const mac = new URLSearchParams(location.search).get("platform") === "mac";
    Object.defineProperty(navigator, "userAgent", {
      configurable: true,
      value: mac ? "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)" : "Mozilla/5.0 (Windows NT 10.0; Win64; x64)",
    });
  });
}

async function inspectNavigationPaint(control, background, foreground) {
  const result = await control.evaluate((element, { background, foreground }) => {
    const style = getComputedStyle(element);
    const probe = document.createElement("span");
    probe.style.backgroundColor = background ? `var(${background})` : "transparent";
    probe.style.color = `var(${foreground})`;
    document.body.append(probe);
    const expected = getComputedStyle(probe);
    const result = { background: style.backgroundColor, color: style.color,
      expectedBackground: expected.backgroundColor, expectedColor: expected.color };
    probe.remove();
    return result;
  }, { background, foreground });
  assert.equal(result.background, result.expectedBackground, "Navigation background must reflect its action or current-location role");
  assert.equal(result.color, result.expectedColor, "Navigation foreground must retain its paired semantic color");
  return result;
}

async function checkNavigationHierarchy(browser) {
  for (const [width, height, font] of [[1280, 800, 13], [1024, 680, 18]]) for (const theme of ["light", "dark"]) {
    const page = await browser.newPage({ viewport: { width, height }, colorScheme: theme });
    page.setDefaultNavigationTimeout(60000);
    const errors = [];
    page.on("pageerror", error => errors.push(error.message));
    try {
      await page.goto(`${base}?surface=expert-entry&extensionState=populated&theme=${theme}`, { waitUntil: "networkidle" });
      const nav = page.getByRole("navigation", { name: "主导航", exact: true });
      const newTask = nav.getByRole("button", { name: "新建任务", exact: true });
      await newTask.waitFor();
      await page.evaluate(font => { document.documentElement.style.fontSize = `${16 * font / 13}px`; }, font);
      await page.mouse.move(width - 8, height - 8);
      assert.equal(await nav.locator(".sidebar__nav-item--active").count(), 0, "Task creation is an action, including on the home page");
      assert.equal(await newTask.getAttribute("aria-current"), null);
      const neutral = await inspectNavigationPaint(newTask, null, "--echo-text-medium");
      await newTask.hover();
      await inspectNavigationPaint(newTask, "--echo-bg-hover", "--echo-text-strong");
      await page.mouse.down();
      await inspectNavigationPaint(newTask, "--echo-bg-active", "--echo-text-strong");
      await page.mouse.move(width - 8, height - 8); await page.mouse.up();
      await newTask.focus();
      await page.keyboard.press("Tab"); await page.keyboard.press("Shift+Tab");
      const focus = await newTask.evaluate(element => ({ focused: element === document.activeElement,
        visible: element.matches(":focus-visible"), outline: getComputedStyle(element).outlineStyle,
        width: parseFloat(getComputedStyle(element).outlineWidth) }));
      assert(focus.focused && focus.visible && focus.outline === "solid" && focus.width >= 2, "Neutral creation must retain visible keyboard focus");

      const capabilities = nav.getByRole("button", { name: "专家·技能·连接器", exact: true });
      await capabilities.click();
      await page.getByRole("heading", { name: "我的专家", exact: true }).waitFor();
      await page.locator(".colleague-card-profile-main").first().waitFor();
      await page.evaluate(() => document.fonts.ready);
      await page.mouse.move(width - 8, height - 8);
      assert.equal(await nav.locator(".sidebar__nav-item--active").count(), 1);
      assert.equal(await capabilities.getAttribute("aria-current"), "page");
      const selected = await inspectNavigationPaint(capabilities, "--echo-control-selected-bg", "--echo-control-selected-fg");
      await inspectNavigationPaint(newTask, null, "--echo-text-medium");
      await capabilities.hover();
      await inspectNavigationPaint(capabilities, "--echo-control-selected-bg-hover", "--echo-control-selected-fg");
      await page.mouse.down();
      await inspectNavigationPaint(capabilities, "--echo-control-selected-bg-active", "--echo-control-selected-fg");
      await page.mouse.move(width - 8, height - 8); await page.mouse.up();

      const createExpert = page.getByRole("button", { name: "创建专家", exact: true });
      const primary = () => createExpert.evaluate(element => ({ background: getComputedStyle(element).backgroundColor, color: getComputedStyle(element).color }));
      for (const state of ["default", "hover", "pressed"]) {
        if (state === "hover") await createExpert.hover();
        if (state === "pressed") await page.mouse.down();
        assertPrimaryAction(await primary(), { theme, state, label: "创建专家" });
      }
      await page.mouse.move(width - 8, height - 8); await page.mouse.up();
      const colors = await page.evaluate(inspectRenderedColors);
      assert.deepEqual([...colors.failures, ...colors.controlFailures], [], "Sidebar hierarchy and expert actions must stay readable");
      await page.screenshot({ path: join(output, `hierarchy-experts-${width}-${font}-${theme}.png`) });
      await createExpert.click();
      const dialog = page.getByRole("dialog", { name: "创建专家", exact: true });
      await dialog.waitFor(); await page.keyboard.press("Escape"); await dialog.waitFor({ state: "hidden" });

      const more = nav.getByRole("button", { name: /更多/ });
      await more.focus(); await page.keyboard.press("ArrowDown");
      const menu = page.getByRole("menu", { name: "更多功能", exact: true });
      await menu.waitFor();
      assert.equal(await nav.locator(".sidebar__nav-item--active").count(), 1, "Opening More must not introduce a second selected module");
      await inspectNavigationPaint(more, "--echo-bg-hover", "--echo-text-strong");
      await inspectNavigationPaint(capabilities, "--echo-control-selected-bg", "--echo-control-selected-fg");
      await menu.getByRole("menuitem", { name: "知识库", exact: true }).click();
      await nav.locator(".sidebar__nav-item--active").filter({ hasText: "更多" }).waitFor();
      assert.equal(await nav.locator(".sidebar__nav-item--active").count(), 1);
      assert.equal(await capabilities.getAttribute("aria-current"), null);
      await page.mouse.move(width - 8, height - 8);
      await inspectNavigationPaint(more, "--echo-control-selected-bg", "--echo-control-selected-fg");
      await inspectNavigationPaint(newTask, null, "--echo-text-medium");
      const projects = nav.getByRole("button", { name: "项目", exact: true });
      await projects.click();
      await page.getByRole("heading", { name: "项目", exact: true }).waitFor();
      assert.equal(await projects.getAttribute("aria-current"), "page");
      assert.equal(await nav.locator(".sidebar__nav-item--active").count(), 1);
      await newTask.focus(); await page.keyboard.press("Enter");
      await page.getByText("今天想完成什么？", { exact: true }).waitFor();
      assert.equal(await page.evaluate(() => window.workbenchNewTask), true, "Neutral new-task action must remain keyboard-operable");
      assert.equal(await nav.locator(".sidebar__nav-item--active").count(), 0);
      await inspectNavigationPaint(newTask, null, "--echo-text-medium");
      assert.deepEqual(errors, []);
      hierarchyEvidence.push({ width, height, font, theme, neutral, selected, focus, primary: "default/hover/pressed", flows: ["create expert dialog", "More stays neutral until navigation", "projects", "keyboard new task"] });
    } finally { await page.close(); }
  }
}

try {
  browser = await (await reviewChromium()).launch({ headless: true, executablePath: process.env.CHROME_BIN || "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" });
  const page = await browser.newPage();
  await emulatePlatform(page);
  page.setDefaultNavigationTimeout(60000);
  const errors = [];
  page.on("pageerror", error => errors.push(error.message));
  for (const [width, height, sidebarWidth, font] of (process.argv.includes("--navigation-only") ? [] : [[1280, 800, 248, 16], [1024, 680, 240, 22], [900, 680, 400, 16]])) {
    for (const theme of ["light", "dark"]) for (const platform of ["mac", "other"]) {
      await page.setViewportSize({ width, height });
      await page.goto(`${base}?surface=home&theme=${theme}&platform=${platform}&windowChrome=1`);
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
          main: rect(document.querySelector(".app__main")),
          toolbar: rect(document.querySelector(".sidebar__toolbar")),
          brand: rect(document.querySelector(".sidebar__logo")),
          brandRow: document.querySelector(".sidebar__logo-row") ? rect(document.querySelector(".sidebar__logo-row")) : null,
          brandCount: document.querySelectorAll(".sidebar__logo").length,
          brandOverflow: document.querySelector(".sidebar__logo").scrollWidth > document.querySelector(".sidebar__logo").clientWidth,
          nav: rect(document.querySelector(".sidebar__nav")),
          tools: [...document.querySelectorAll(".sidebar__toolbar-actions button")].map(element => ({ label: element.getAttribute("aria-label"), ...rect(element) })),
          toggle: rect(document.querySelector(".window-sidebar-toggle")),
          duplicateCollapse: document.querySelectorAll('button[aria-label="收起侧边栏"]').length,
          overflow: document.documentElement.scrollWidth > innerWidth + 1,
        };
      });
      assert.equal(geometry.overflow, false, `${name}: page overflow`);
      assert.equal(geometry.main.y, platform === "mac" ? 0 : 32, `${name}: main header starts directly below native chrome`);
      assert.equal(geometry.main.bottom, height, `${name}: main column fills the window`);
      assert.equal(geometry.toolbar.height, 56, `${name}: toolbar must leave breathing room`);
      assert.equal(geometry.duplicateCollapse, 1, `${name}: one collapse control`);
      assert.equal(geometry.brandCount, 1, `${name}: one brand wordmark`);
      assert.equal(geometry.brandOverflow, false, `${name}: keep the full brand wordmark readable`);
      assert.deepEqual(geometry.tools.map(tool => tool.label), ["搜索全部任务", "筛选任务"]);
      assert.equal(geometry.toggle.width, 32, `${name}: toggle width`);
      assert.equal(geometry.toggle.height, 32, `${name}: toggle height`);
      assert.equal(geometry.toggle.y, geometry.tools[0].y, `${name}: window and sidebar controls share a center line`);
      assert.equal(geometry.tools[0].x - geometry.toggle.right, 4, `${name}: keep the toggle, search and filter in one compact group`);
      for (const [index, tool] of geometry.tools.entries()) {
        assert.ok(tool.width === 32 && tool.height === 32 && tool.right <= geometry.sidebar.right - 16, `${name}: inconsistent tool targets`);
        assert.equal(tool.y - geometry.toolbar.y, 12, `${name}: toolbar top inset`);
        assert.equal(geometry.toolbar.bottom - tool.bottom, 12, `${name}: toolbar bottom inset`);
        assert.equal(tool.y, geometry.tools[0].y, `${name}: tools must align`);
        if (index) assert.equal(tool.x - geometry.tools[index - 1].right, 4, `${name}: tool spacing`);
      }
      assert.equal(geometry.brandRow, null, `${name}: branding must share the tools row`);
      assert.ok(geometry.brand.y >= geometry.toolbar.y && geometry.brand.bottom <= geometry.toolbar.bottom, `${name}: branding stays inside the toolbar`);
      assert.ok(Math.abs(geometry.brand.y + geometry.brand.height / 2 - geometry.toggle.y - geometry.toggle.height / 2) <= 1, `${name}: brand and tools must share a center line`);
      assert.ok(geometry.brand.right <= geometry.toggle.x - 8, `${name}: branding must not overlap tools`);
      assert.equal(geometry.nav.y, geometry.toolbar.bottom, `${name}: navigation moves directly below the unified header`);
      assert.equal(geometry.toolbar.y, 32, `${name}: native window row reservation`);
      if (platform === "mac") {
        // AppKit controls center 2pt below the configured inset. Keep the entire
        // application row below those native buttons, with a usable drag strip.
        assert.ok(geometry.toggle.y >= macWindow.trafficLightPosition.y + 2 + 7 + 8, `${name}: toolbar overlaps native window controls`);
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
      const persistentToggle = await collapse.elementHandle();
      const persistentBrand = await page.locator(".window-sidebar-brand").elementHandle();
      const brandAnchor = await persistentBrand.boundingBox();
      await collapse.focus();
      await collapse.press("Enter");
      const expand = page.getByRole("button", { name: "展开侧边栏", exact: true });
      await expand.waitFor();
      assert.ok(await expand.evaluate(element => element === document.activeElement), `${name}: collapse focus transfer`);
      assert.deepEqual(await page.locator(".window-sidebar-navigation button").evaluateAll(buttons => buttons.map(button => button.getAttribute("aria-label"))), ["展开侧边栏", "新建任务"]);
      assert.equal(await page.locator(".main-page-controls button").count(), 0, `${name}: page must not duplicate window controls`);
      assert.ok(await expand.evaluate((element, original) => element === original, persistentToggle), `${name}: toggle must remain mounted`);
      const collapsedToggle = await expand.boundingBox();
      assert.equal(await page.locator(".sidebar__logo").count(), 1, `${name}: collapse duplicated branding`);
      assert.ok(await page.locator(".window-sidebar-brand").evaluate((element, original) => element === original, persistentBrand), `${name}: collapse replaced branding`);
      if (platform === "mac") {
        const brand = await page.locator(".window-sidebar-brand").boundingBox();
        const header = await page.locator(".main-page-controls").boundingBox();
        assert.ok(brand.x >= 92, `${name}: collapsed branding clears native buttons`);
        assert.ok(brand.x + brand.width <= collapsedToggle.x, `${name}: branding clears the toggle`);
        assert.ok(collapsedToggle.y >= header.y && collapsedToggle.y + collapsedToggle.height <= header.y + header.height, `${name}: collapsed navigation stays inside the top header`);
      } else {
        assert.equal(collapsedToggle.x, geometry.toggle.x, `${name}: toggle jumped horizontally`);
        assert.equal(collapsedToggle.y, geometry.toggle.y, `${name}: toggle jumped vertically`);
        assert.deepEqual(await page.locator(".window-sidebar-brand").boundingBox(), brandAnchor, `${name}: collapse moved branding`);
      }
      await page.getByRole("button", { name: "新建任务", exact: true }).click();
      assert.ok(await page.evaluate(() => window.workbenchNewTask), `${name}: new task callback`);
      assert.ok(await page.locator(".echo-composer").isVisible(), `${name}: collapsed page content`);
      await page.mouse.move(width - 10, height - 10);
      await page.screenshot({ path: join(output, `collapsed-${name}.png`) });
      await expand.click();
      assert.ok(await collapse.evaluate(element => element === document.activeElement), `${name}: expand focus return`);
      assert.equal(await menu.count(), 0, `${name}: stale filter menu after collapse`);
      // Collapse and restore remain reachable; the expanded sidebar stays fixed.
      for (let turn = 0; turn < 4; turn++) {
        const target = await page.locator(".window-sidebar-toggle").boundingBox();
        await page.mouse.click(target.x + 16, target.y + 16);
        assert.equal(await page.locator(".window-sidebar-toggle").getAttribute("aria-expanded"), turn % 2 ? "true" : "false", `${name}: repeated click missed the control`);
      }
      assert.deepEqual(await collapse.boundingBox(), { x:geometry.toggle.x, y:geometry.toggle.y, width:geometry.toggle.width, height:geometry.toggle.height }, `${name}: restoring changed the sidebar toolbar position`);
      await persistentToggle.dispose();
      await persistentBrand.dispose();
      evidence.push({ name, ...geometry, menu: bounds });
    }
  }

  // Exercise the largest supported navigation configuration, all three
  // catalogs and the real font preference maximum, including the 32px
  // Windows/Linux title bar that reduces the available sidebar height.
  const catalogPage = await browser.newPage();
  await emulatePlatform(catalogPage);
  catalogPage.setDefaultNavigationTimeout(60000);
  catalogPage.on("pageerror", error => errors.push(error.message));
  await catalogPage.addInitScript(() => localStorage.setItem("echoagent.sidebar-menu.v1", JSON.stringify({
    primary: ["projects", "organization", "coding", "capabilities", "knowledge", "automation"], more: [],
  })));
  for (const height of (process.argv.includes("--navigation-only") ? [] : [600, 680, 760, 800])) for (const font of [16, 16 * 18 / 13]) {
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
  await checkNavigationHierarchy(browser);
  const codingPage = await browser.newPage({ viewport: { width: 1280, height: 800 } });
  await emulatePlatform(codingPage);
  codingPage.on("pageerror", error => errors.push(error.message));
  await codingPage.goto(`${base}?surface=coding&codingShell=1&platform=mac`);
  const toggle = codingPage.locator(".window-sidebar-toggle");
  await toggle.waitFor();
  const original = await toggle.elementHandle();
  const anchor = await toggle.boundingBox();
  const divider = await codingPage.getByRole("separator", { name: "调整侧边栏宽度", exact: true }).boundingBox();
  await codingPage.mouse.move(divider.x + divider.width / 2, 250);
  await codingPage.mouse.down();
  await codingPage.mouse.move(360, 250, { steps: 8 });
  await codingPage.mouse.up();
  assert.deepEqual(await toggle.boundingBox(), anchor, "resizing the sidebar moved window navigation");
  await codingPage.mouse.click(anchor.x + 16, anchor.y + 16);
  await codingPage.getByRole("button", { name: "展开侧边栏", exact: true }).waitFor();
  assert.equal(await codingPage.getByRole("button", { name: "展开侧边栏", exact: true }).count(), 1, "coding page duplicated sidebar restoration");
  const collapsedAnchor = await toggle.boundingBox();
  const identity = await codingPage.locator(".coding-workbench__topbar-left").boundingBox();
  assert.ok(identity.x >= collapsedAnchor.x + 68, "coding identity overlaps window controls");
  await codingPage.screenshot({ path: join(output, "coding-collapsed-fixed-navigation.png") });
  await codingPage.getByRole("button", { name: "新建任务", exact: true }).click();
  await codingPage.locator(".app__coding-surface[hidden]").waitFor({ state: "attached" });
  assert.ok(await toggle.evaluate((element, original) => element === original, original), "changing the page replaced window navigation");
  assert.deepEqual(await toggle.boundingBox(), collapsedAnchor, "changing the page moved window navigation");
  await codingPage.emulateMedia({ reducedMotion: "reduce" });
  await codingPage.mouse.click(collapsedAnchor.x + 16, collapsedAnchor.y + 16);
  await codingPage.getByRole("button", { name: "代码开发", exact: true }).click();
  await codingPage.locator(".app__coding-surface:not([hidden])").waitFor();
  assert.ok(await toggle.evaluate((element, original) => element === original, original), "returning to coding replaced window navigation");
  assert.deepEqual(await toggle.boundingBox(), anchor, "returning to coding moved window navigation");
  await original.dispose();
  await codingPage.close();
  assert.deepEqual(errors, [], "sidebar toolbar page errors");
  writeFileSync(join(output, "report.json"), JSON.stringify({ passed: true, evidence, catalogEvidence, hierarchyEvidence }, null, 2));
  console.log(`Sidebar toolbar passed: ${evidence.length * 2 + catalogEvidence.length} layouts, ${hierarchyEvidence.length} navigation hierarchy scenarios, search, filters, new task, catalog scrolling and keyboard focus; fixed navigation across coding, resize and page changes.`);
} finally {
  await browser?.close();
  await server.close();
}
