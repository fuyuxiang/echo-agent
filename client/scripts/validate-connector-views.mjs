import assert from "node:assert/strict";
import { join } from "node:path";

/** Real connector components and CSS, with task-bound IPC isolated in memory. */
export async function validateConnectorViews(browser, base, output) {
  const evidence = [];
  for (const [width, height, font, theme] of [
    [1440, 900, 16, "light"], [1280, 800, 16, "dark"],
    [1024, 680, 22, "dark"], [768, 720, 22, "light"],
    [390, 680, 16, "light"], [350, 720, 22, "dark"],
  ]) {
    const page = await browser.newPage({ viewport: { width, height } });
    const errors = [];
    page.on("pageerror", error => errors.push(error.message));
    page.setDefaultNavigationTimeout(60000);
    try {
      const query = new URLSearchParams({ surface: "capabilities", label: "连接器", extensionState: "populated", theme,
        extensionShell: width >= 1024 ? "1" : "0" });
      const open = async () => {
        await page.goto(`${base}?${query}`, { waitUntil: "networkidle" });
        await page.locator(".mcp-item-wrap").first().waitFor();
        await page.evaluate(font => { document.documentElement.style.fontSize = `${font}px`; }, font);
      };
      await open();
      assert.equal(await page.locator(".mcp-list").getAttribute("data-view"), "list", "New connector pages default to list");
      await page.evaluate(() => {
        const make = (name, extra = {}) => ({ name, transport: "streamable_http", target: `https://review.invalid/${name}/mcp`,
          enabled: true, source: "user", status: "ready", live: false, authRequired: false, setupRequired: false,
          setupValues: {}, tools: [], args: [], env: {}, editable: true, ...extra });
        const tool = { name: "search", displayName: "搜索资料", description: "根据当前任务检索工作区中的相关资料。".repeat(8), enabled: true };
        const state = { calls: [], entries: [
          make("echoagent"), make("echoagent-automation"),
          make("workspace-search", { displayName: "工作区检索", live: true, tools: [tool] }),
          make("oauth-demo", { displayName: "需授权的知识服务", authRequired: true }),
        ] };
        window.connectorViewReview = state;
        const original = window.__TAURI_INTERNALS__.invoke;
        window.__TAURI_INTERNALS__.invoke = async (command, args = {}) => {
          if (command === "mcp_list") return state.entries;
          if (command === "mcp_toggle") {
            state.calls.push({ command, args });
            state.entries = state.entries.map(entry => entry.name === args.name ? { ...entry, enabled: args.enabled } : entry);
            return { warnings: [] };
          }
          if (command === "mcp_auth_trigger") {
            state.calls.push({ command, args });
            state.entries = state.entries.map(entry => entry.name === args.serverName ? { ...entry, authRequired: false, live: true, tools: [tool] } : entry);
            return { status: "authenticated" };
          }
          if (command === "mcp_toggle_tool") {
            state.calls.push({ command, args });
            state.entries = state.entries.map(entry => entry.name === args.serverName
              ? { ...entry, tools: entry.tools.map(tool => tool.name === args.toolName ? { ...tool, enabled: args.enabled } : tool) } : entry);
            return;
          }
          return original(command, args);
        };
      });
      await page.getByRole("button", { name: "刷新诊断", exact: true }).click();
      await page.waitForFunction(() => document.querySelectorAll(".mcp-item-wrap").length === 4);
      assert.equal(await page.getByText("待任务启动", { exact: true }).count(), 2);
      assert.equal(await page.getByText("0 个工具", { exact: true }).count(), 0);
      assert.equal(await page.getByText("已启用，启动任务后连接", { exact: true }).count(), 0);

      const layouts = {};
      for (const view of ["list", "cards"]) {
        const button = page.getByRole("button", { name: view === "cards" ? "卡片视图" : "列表视图", exact: true });
        await button.focus();
        await button.press("Enter");
        assert.ok(await button.evaluate(element => element === document.activeElement));
        const geometry = await page.locator(".mcp-list").evaluate(list => {
          const box = element => {
            const { left, right, top, bottom, width, height } = element.getBoundingClientRect();
            return { left, right, top, bottom, width, height };
          };
          return { ...box(list), view: list.dataset.view, columns: getComputedStyle(list).gridTemplateColumns.split(" ").length,
            overflow: document.documentElement.scrollWidth > innerWidth + 1 || list.scrollWidth > list.clientWidth + 1,
            rows: [...list.querySelectorAll(":scope > .mcp-item-wrap")].map(row => ({ ...box(row),
              overflow: row.scrollWidth > row.clientWidth + 1,
              cells: [".mcp-item-main", ".mcp-item-transport", ".mcp-item-health", ".resource-row__actions"].map(selector => box(row.querySelector(selector))),
            })) };
        });
        assert.equal(geometry.view, view);
        assert.equal(geometry.overflow, false, `${width}/${font}/${view}: collection overflow`);
        assert.equal(geometry.rows.length, 4);
        for (const row of geometry.rows) {
          assert.equal(row.overflow, false);
          for (const [index, cell] of row.cells.entries()) {
            assert.ok(cell.left >= row.left - 1 && cell.right <= row.right + 1, "Connector cells must fit their card or row");
            for (const previous of row.cells.slice(0, index)) assert.ok(
              cell.left >= previous.right - 1 || cell.right <= previous.left + 1 || cell.top >= previous.bottom - 1 || cell.bottom <= previous.top + 1,
              `${width}/${view}: connector fields overlap`);
          }
          if (view === "cards") assert.ok(row.cells[3].top >= Math.max(...row.cells.slice(0, 3).map(cell => cell.bottom)), "Card actions belong below the information");
        }
        if (view === "cards" && geometry.width >= 40 * font) assert.ok(geometry.columns >= 2, "Wide connector cards must use multiple columns");
        if (view === "list" && geometry.width > 52 * font) {
          for (const column of [1, 2, 3]) assert.ok(geometry.rows.every(row => Math.abs(row.cells[column].left - geometry.rows[0].cells[column].left) <= 1), "List columns must align across connectors");
          assert.ok(geometry.rows[0].height <= 80, "List view must remain compact at the standard font size");
        }
        layouts[view] = geometry;
        evidence.push({ viewport: { width, height }, font, theme, ...geometry });
        await page.mouse.move(width - 4, height - 4);
        await page.screenshot({ path: join(output, `connector-${width}-${font}-${theme}-${view}.png`) });
      }
      assert.ok(layouts.cards.rows[0].height >= layouts.list.rows[0].height + 24, "The two views must have a meaningful density difference");
      assert.equal(await page.evaluate(() => window.connectorViewReview.calls.length), 0, "Changing views must not mutate any connector");

      const first = page.locator(".mcp-item-main").filter({ hasText: /^echoagent$/ });
      await first.click();
      const detailId = await first.getAttribute("aria-controls");
      await page.getByText("启动任务后将连接服务并获取工具", { exact: true }).waitFor();
      await page.getByRole("button", { name: "列表视图", exact: true }).click();
      assert.equal(await first.getAttribute("aria-expanded"), "true");
      assert.equal(await first.getAttribute("aria-controls"), detailId);
      assert.ok(await page.locator(".mcp-item-details").isVisible());
      await first.click();

      for (const view of ["卡片视图", "列表视图"]) {
        await page.getByRole("button", { name: view, exact: true }).click();
        await page.getByRole("checkbox", { name: "停用 MCP 服务 echoagent", exact: true }).click();
        await page.getByRole("checkbox", { name: "启用 MCP 服务 echoagent", exact: true }).waitFor();
        await page.getByRole("checkbox", { name: "启用 MCP 服务 echoagent", exact: true }).click();
        await page.getByRole("checkbox", { name: "停用 MCP 服务 echoagent", exact: true }).waitFor();
      }
      await page.getByRole("button", { name: "卡片视图", exact: true }).click();
      await page.getByRole("button", { name: "授权 MCP 服务 需授权的知识服务", exact: true }).click();
      await page.getByRole("button", { name: "授权 MCP 服务 需授权的知识服务", exact: true }).waitFor({ state: "hidden" });
      await page.locator(".mcp-item-main").filter({ hasText: "工作区检索" }).click();
      for (const [view, action, nextAction] of [["卡片视图", "停用", "启用"], ["列表视图", "启用", "停用"]]) {
        await page.getByRole("button", { name: view, exact: true }).click();
        await page.getByRole("checkbox", { name: `${action}工具 搜索资料`, exact: true }).click();
        await page.getByRole("checkbox", { name: `${nextAction}工具 搜索资料`, exact: true }).waitFor();
        const detail = page.locator(".mcp-item-details");
        await page.screenshot({ path: join(output, `connector-${width}-${font}-${theme}-${view}-details.png`) });
        const detailGeometry = await detail.evaluate(element => ({
          width: element.clientWidth, scrollWidth: element.scrollWidth,
          overflowing: [...element.querySelectorAll("*")].filter(child => child.getBoundingClientRect().right > element.getBoundingClientRect().right + 1)
            .map(child => ({ tag: child.tagName, className: child.className, width: child.getBoundingClientRect().width, columns: getComputedStyle(child).gridTemplateColumns })),
        }));
        assert.ok(detailGeometry.scrollWidth <= detailGeometry.width + 1, `Expanded tools must fit: ${width}/${font}/${view} ${JSON.stringify(detailGeometry)}`);
      }

      await page.getByRole("button", { name: "卡片视图", exact: true }).click();
      const more = page.getByRole("button", { name: "连接器 echoagent更多操作", exact: true });
      await more.click();
      const menu = page.getByRole("menu", { name: "连接器 echoagent操作菜单", exact: true });
      await menu.waitFor();
      const bounds = await menu.boundingBox();
      assert.ok(bounds.x >= 0 && bounds.y >= 0 && bounds.x + bounds.width <= width + 1 && bounds.y + bounds.height <= height + 1, "Card menus must not be clipped");
      await page.keyboard.press("Escape");
      assert.ok(await more.evaluate(element => element === document.activeElement));
      const search = page.getByRole("textbox", { name: "搜索连接器", exact: true });
      await search.fill("没有这个连接器");
      await page.getByRole("button", { name: "清空搜索", exact: true }).click();
      await page.locator('.mcp-list[data-view="cards"]').waitFor();
      assert.equal(await page.locator(".mcp-item-wrap").count(), 4);
      await open();
      assert.equal(await page.locator(".mcp-list").getAttribute("data-view"), "cards", "Reload must preserve a user's saved card preference");
      assert.deepEqual(errors, []);
    } finally {
      await page.close();
    }
  }
  return evidence;
}
