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
      await page.locator(".app__body").evaluate((element, width) => element.style.setProperty("--app-sidebar-width", `${width}px`), sidebarWidth);
      await page.evaluate(font => { document.documentElement.style.fontSize = `${font}px`; }, font);
      const name = `${width}-${sidebarWidth}-${font}-${theme}-${platform}`;
      const geometry = await page.evaluate(() => {
        const rect = element => {
          const { x, y, right, bottom, width, height } = element.getBoundingClientRect();
          return { x, y, right, bottom, width, height };
        };
        const caption = document.querySelector(".titlebar--caption-only");
        return {
          sidebar: rect(document.querySelector(".sidebar")),
          main: rect(document.querySelector(".app__main")),
          toolbar: rect(document.querySelector(".window-sidebar-navigation")),
          brand: rect(document.querySelector(".sidebar__logo")),
          brandRow: document.querySelector(".sidebar__logo-row") ? rect(document.querySelector(".sidebar__logo-row")) : null,
          brandCount: document.querySelectorAll(".sidebar__logo").length,
          brandOverflow: document.querySelector(".sidebar__logo").scrollWidth > document.querySelector(".sidebar__logo").clientWidth,
          nav: rect(document.querySelector(".sidebar__nav")),
          search: rect(document.querySelector(".sidebar__task-search")),
          filter: rect(document.querySelector(".sidebar__list-section--tasks .task-filter-trigger")),
          taskHeading: rect(document.querySelector(".sidebar__list-section--tasks .sidebar__section-head")),
          caption: caption ? rect(caption) : null,
          toggle: rect(document.querySelector(".window-sidebar-toggle")),
          duplicateCollapse: document.querySelectorAll('button[aria-label="收起侧边栏"]').length,
          overflow: document.documentElement.scrollWidth > innerWidth + 1,
        };
      });
      assert.equal(geometry.overflow, false, `${name}: page overflow`);
      assert.equal(geometry.main.y, 0, `${name}: main header shares the window row`);
      assert.equal(geometry.main.bottom, height, `${name}: main column fills the window`);
      assert.equal(geometry.toolbar.height, 52, `${name}: shared window row height`);
      assert.equal(geometry.duplicateCollapse, 1, `${name}: one collapse control`);
      assert.equal(geometry.brandCount, 1, `${name}: one brand wordmark`);
      assert.equal(geometry.brandOverflow, false, `${name}: keep the full brand wordmark readable`);
      assert.equal(geometry.toggle.width, 32, `${name}: toggle width`);
      assert.equal(geometry.toggle.height, 32, `${name}: toggle height`);
      assert.equal(geometry.brandRow, null, `${name}: no duplicate wordmark row`);
      assert.ok(geometry.brand.y >= geometry.toolbar.y && geometry.brand.bottom <= geometry.toolbar.bottom, `${name}: branding stays inside the toolbar`);
      assert.equal(geometry.brand.x, geometry.toggle.right + 8, `${name}: wordmark follows the toggle`);
      assert.ok(Math.abs(geometry.brand.y + geometry.brand.height / 2 - 26) <= 1, `${name}: wordmark aligns with the toggle`);
      assert.ok(geometry.search.y >= geometry.toolbar.bottom && geometry.search.bottom <= geometry.nav.y, `${name}: task search belongs above the navigation`);
      assert.ok(geometry.filter.y >= geometry.taskHeading.y && geometry.filter.bottom <= geometry.taskHeading.bottom, `${name}: filter belongs in the task heading`);
      if (platform === "mac") {
        assert.equal(geometry.toggle.y + geometry.toggle.height / 2, macWindow.trafficLightPosition.y, `${name}: align with the native controls`);
        assert.ok(geometry.toggle.x >= macWindow.trafficLightPosition.x + 64, `${name}: toolbar overlaps native window controls`);
      } else {
        assert.equal(geometry.toggle.y, 10, `${name}: toggle centered in the title bar`);
        assert.equal(geometry.caption.bottom, geometry.toolbar.bottom, `${name}: Windows controls share the title bar`);
        assert.equal(geometry.caption.right, width, `${name}: Windows controls stay at the right edge`);
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
      const emptyTaskSection = page.getByRole("button", { name: "任务 (0)", exact: true });
      await emptyTaskSection.click();
      assert.equal(await emptyTaskSection.getAttribute("aria-expanded"), "false");
      assert.equal(await page.getByText("暂无任务", { exact: true }).isVisible(), false);
      await emptyTaskSection.click();
      assert.equal(await page.getByText("暂无任务", { exact: true }).isVisible(), true);
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
      for (const key of ["Tab", "Shift+Tab"]) {
        await filter.focus();
        await page.keyboard.press(key);
        const adjacent = await page.evaluateHandle(() => document.activeElement);
        await filter.focus();
        await page.keyboard.press("ArrowDown");
        await menu.waitFor();
        await page.keyboard.press(key);
        await menu.waitFor({ state: "hidden" });
        assert.ok(await adjacent.evaluate(element => element === document.activeElement), `${name}: ${key} should continue from the sidebar trigger`);
        await adjacent.dispose();
      }

      const collapse = page.getByRole("button", { name: "收起侧边栏", exact: true });
      const persistentToggle = await collapse.elementHandle();
      const brand = page.locator(".window-sidebar-brand");
      await collapse.focus();
      await collapse.press("Enter");
      const expand = page.getByRole("button", { name: "展开侧边栏", exact: true });
      await expand.waitFor();
      assert.ok(await expand.evaluate(element => element === document.activeElement), `${name}: collapse focus transfer`);
      assert.deepEqual(await page.locator(".window-sidebar-navigation button:visible").evaluateAll(buttons => buttons.map(button => button.getAttribute("aria-label"))), ["展开侧边栏", "搜索全部任务", "新建任务"]);
      assert.equal(await page.locator(".main-page-controls button").count(), 0, `${name}: page must not duplicate window controls`);
      assert.ok(await expand.evaluate((element, original) => element === original, persistentToggle), `${name}: toggle must remain mounted`);
      assert.equal(await brand.isVisible(), false, `${name}: collapsed sidebar still shows the brand`);
      assert.deepEqual(await expand.boundingBox(), { x:geometry.toggle.x, y:geometry.toggle.y, width:geometry.toggle.width, height:geometry.toggle.height }, `${name}: collapse moved the toggle`);
      const collapsedSearch = page.locator(".window-sidebar-quick-actions").getByRole("button", { name: "搜索全部任务", exact: true });
      const searchBox = await collapsedSearch.boundingBox();
      assert.equal(searchBox.x, geometry.toggle.right + 2, `${name}: search sits next to the toggle`);
      await collapsedSearch.click();
      await page.getByRole("dialog", { name: "搜索会话", exact: true }).waitFor();
      await page.keyboard.press("Escape");
      await page.getByRole("dialog", { name: "搜索会话", exact: true }).waitFor({ state: "hidden" });
      assert.ok(await collapsedSearch.evaluate(element => element === document.activeElement), `${name}: collapsed search focus return`);
      const newTask = page.getByRole("button", { name: "新建任务", exact: true });
      assert.equal((await newTask.boundingBox()).x, searchBox.x + searchBox.width + 2, `${name}: new task sits next to search`);
      await newTask.click();
      assert.ok(await page.evaluate(() => window.workbenchNewTask), `${name}: new task callback`);
      assert.ok(await page.locator(".echo-composer").isVisible(), `${name}: collapsed page content`);
      await page.mouse.move(width - 10, height - 10);
      await page.screenshot({ path: join(output, `collapsed-${name}.png`) });
      await expand.click();
      assert.ok(await collapse.evaluate(element => element === document.activeElement), `${name}: expand focus return`);
      assert.equal(await menu.count(), 0, `${name}: stale filter menu after collapse`);
      for (const [control, prior] of [[search, geometry.search], [filter, geometry.filter]]) {
        assert.deepEqual(await control.boundingBox(), { x: prior.x, y: prior.y, width: prior.width, height: prior.height }, `${name}: restoring moved a toolbar control`);
      }
      // Collapse and restore remain reachable; the expanded sidebar stays fixed.
      for (let turn = 0; turn < 4; turn++) {
        const target = await page.locator(".window-sidebar-toggle").boundingBox();
        await page.mouse.click(target.x + 16, target.y + 16);
        assert.equal(await page.locator(".window-sidebar-toggle").getAttribute("aria-expanded"), turn % 2 ? "true" : "false", `${name}: repeated click missed the control`);
      }
      assert.deepEqual(await collapse.boundingBox(), { x:geometry.toggle.x, y:geometry.toggle.y, width:geometry.toggle.width, height:geometry.toggle.height }, `${name}: restoring changed the sidebar toolbar position`);
      await persistentToggle.dispose();
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
      await catalogPage.goto(`${base}?surface=home&sidebarCatalog=both&groups=12&windowChrome=1&theme=${theme}&platform=${platform}`);
      await catalogPage.locator(".sidebar__content[data-catalog-ready]").waitFor();
      await catalogPage.evaluate(font => { document.documentElement.style.fontSize = `${font}px`; }, font);
      await catalogPage.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
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
          })),
        };
      });
      const context = `${name}: ${JSON.stringify(geometry)}`;
      assert.equal(geometry.overflow, false, `catalog horizontal overflow: ${context}`);
      assert.equal(geometry.lists.length, 3);
      assert.ok(geometry.content.bottom <= geometry.footer.y + 1, `catalog overlaps account controls: ${context}`);
      const catalogFits = geometry.content.scrollHeight <= geometry.content.clientHeight + 1;
      for (const list of geometry.lists) {
        assert.ok(list.height >= list.firstRow.height - 1, `catalog must retain a complete first row: ${context}`);
        if (catalogFits || height >= macWindow.minHeight) {
          assert.ok(list.firstRow.y >= geometry.content.y && list.firstRow.bottom <= geometry.content.bottom + 1, `every first row must be visible at supported heights: ${context}`);
        }
      }
      await catalogPage.getByRole("button", { name: "筛选任务", exact: true }).click();
      const menu = catalogPage.getByRole("menu", { name: "任务筛选", exact: true });
      assert.ok(await menu.evaluate(element => {
        const box = element.getBoundingClientRect();
        return element.contains(document.elementFromPoint(box.x + box.width / 2, box.y + 12));
      }), `catalog scrolling must not clip the toolbar filter: ${context}`);
      await catalogPage.keyboard.press("Escape");
      // When all sections and their footer controls exceed the available space,
      // focusing the last disclosure must scroll it fully into view.
      const groupsMore = catalogPage.locator('.sidebar__catalog-more[aria-controls="sidebar-group-list"]');
      const groupsMoreVisible = () => groupsMore.evaluate(button => {
        const box = button.getBoundingClientRect();
        const content = button.closest(".sidebar__content").getBoundingClientRect();
        return box.top >= content.top && box.bottom <= content.bottom + 1
          && button.contains(document.elementFromPoint(box.x + box.width / 2, box.y + box.height / 2));
      });
      if (catalogFits || height >= macWindow.minHeight) {
        assert.ok(await groupsMoreVisible(), `More groups must be fully visible at supported heights: ${context}`);
      }
      const firstHidden = await catalogPage.locator("#sidebar-group-list [inert] .group-sidebar__item > span").first().innerText();
      await groupsMore.focus();
      assert.ok(await groupsMoreVisible(), `Focusing More groups must reveal the full control: ${context}`);
      await catalogPage.keyboard.press("Enter");
      await catalogPage.locator("#sidebar-group-list:not([data-catalog-preview])").waitFor();
      assert.equal(await catalogPage.locator("#sidebar-group-list .group-sidebar__item:focus > span").innerText(), firstHidden);
      await catalogPage.locator("#sidebar-group-list .group-sidebar__item").last().focus();
      assert.ok(await catalogPage.locator("#sidebar-group-list .group-sidebar__item").last().evaluate(element => {
        const box = element.getBoundingClientRect();
        const content = element.closest(".sidebar__content").getBoundingClientRect();
        return box.y >= content.y - 1 && box.bottom <= content.bottom + 1;
      }), `final catalog item must be reachable by keyboard: ${context}`);
      await catalogPage.keyboard.press("Escape");
      assert.ok(await groupsMore.evaluate(button => button === document.activeElement));
      await catalogPage.locator(".sidebar__content").evaluate(element => { element.scrollTop = 0; });
      await catalogPage.locator("#sidebar-group-list").evaluate(element => { element.scrollTop = 0; });
      await catalogPage.screenshot({ path: join(output, `${name}.png`) });
      catalogEvidence.push({ name, ...geometry });
    }
  }
  if (!process.argv.includes("--navigation-only")) {
    await catalogPage.setViewportSize({ width: 1024, height: 800 });
    await catalogPage.goto(`${base}?surface=home&sidebarCatalog=both&groups=12&windowChrome=1&theme=light&platform=mac`);
    const disclosure = catalogPage.locator('.sidebar__catalog-more[aria-controls="sidebar-group-list"]');
    await catalogPage.locator('.sidebar__catalog-more:not(.sidebar__catalog-more--compact)[aria-controls="sidebar-group-list"]').waitFor();
    await disclosure.focus();
    for (const height of [680, 800]) {
      await catalogPage.setViewportSize({ width: 1024, height });
      await catalogPage.waitForFunction(compact => {
        const button = document.querySelector('.sidebar__catalog-more[aria-controls="sidebar-group-list"]');
        return button && document.activeElement === button
          && button.classList.contains("sidebar__catalog-more--compact") === compact;
      }, height === 680, { timeout: 5000 });
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
  const initialToggle = await toggle.boundingBox();
  const sidebar = codingPage.locator(".sidebar");
  const initialSidebar = await sidebar.boundingBox();
  const divider = await codingPage.getByRole("separator", { name: "调整侧边栏宽度", exact: true }).boundingBox();
  await codingPage.mouse.move(divider.x + divider.width / 2, 250);
  await codingPage.mouse.down();
  await codingPage.mouse.move(360, 250, { steps: 8 });
  await codingPage.mouse.up();
  const anchor = await toggle.boundingBox();
  const resizedSidebar = await sidebar.boundingBox();
  assert.ok(resizedSidebar.width > initialSidebar.width, "sidebar did not resize");
  assert.deepEqual(anchor, initialToggle, "resizing the sidebar moved the window toolbar");
  await codingPage.mouse.click(anchor.x + 16, anchor.y + 16);
  await codingPage.getByRole("button", { name: "展开侧边栏", exact: true }).waitFor();
  assert.equal(await codingPage.getByRole("button", { name: "展开侧边栏", exact: true }).count(), 1, "coding page duplicated sidebar restoration");
  const collapsedAnchor = await toggle.boundingBox();
  const identity = await codingPage.locator(".coding-workbench__topbar-left").boundingBox();
  const windowTools = await codingPage.locator(".window-sidebar-navigation").boundingBox();
  assert.ok(identity.x >= windowTools.x + windowTools.width, "coding identity overlaps window controls");
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
  console.log(`Sidebar toolbar passed: ${evidence.length * 2 + catalogEvidence.length} layouts, ${hierarchyEvidence.length} navigation hierarchy scenarios, search, filters, new task, catalog scrolling and keyboard focus; toolbar alignment across resizing and page changes.`);
} finally {
  await browser?.close();
  await server.close();
}
