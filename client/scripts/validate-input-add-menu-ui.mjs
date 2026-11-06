import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createUiReviewServer, reviewChromium, reviewOutput } from "./ui-review-runtime.mjs";

const output = reviewOutput(mkdtempSync(join(tmpdir(), "echo-input-add-menu-")));
const server = await createUiReviewServer(0, { fixture: "/scripts/fixtures/input-add-menu-review.tsx", freezeMotion: false });
const base = `http://127.0.0.1:${server.httpServer.address().port}/__ui-review`;
let browser, activePage;
const evidence = [], errors = [];
const fits = async (page, menu) => {
  const box = await menu.boundingBox(), viewport = page.viewportSize();
  assert(box && box.x >= 7 && box.y >= 7 && box.x + box.width <= viewport.width - 7 && box.y + box.height <= viewport.height - 7, `menu outside viewport: ${JSON.stringify(box)}`);
  assert(await menu.evaluate(el => el.scrollWidth <= el.clientWidth + 1), "menu has horizontal overflow");
};
const pointerClick = async (page, control) => {
  await control.waitFor();
  const box = await control.boundingBox();
  assert(box, "pointer target is visible");
  const point = { x: box.x + box.width / 2, y: box.y + box.height / 2 };
  assert(await control.evaluate((el, point) => el.contains(document.elementFromPoint(point.x, point.y)), point), "pointer target is not covered");
  // Locator.click scrolls every ancestor before clicking. Raw pointer input
  // verifies the user's actual geometry without that automation side effect.
  await page.mouse.click(point.x, point.y);
};
try {
  browser = await (await reviewChromium()).launch({ headless: true, executablePath: process.env.CHROME_BIN || "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" });
  for (const width of [350, 390, 768, 1280]) for (const font of [16, 22]) for (const theme of ["light", "dark"]) {
    const page = await browser.newPage({ viewport: { width, height: 800 } });
    activePage = page;
    page.on("pageerror", error => errors.push(error.message));
    await page.goto(`${base}?theme=${theme}`);
    await page.evaluate(font => { document.documentElement.style.fontSize = `${font}px`; }, font);
    await page.getByRole("button", { name: "添加", exact: true }).click();
    const main = page.getByRole("menu", { name: "添加内容", exact: true });
    await fits(page, main);
    const experts = page.getByRole("menuitem", { name: "专家", exact: true });
    await experts.hover();
    // Narrow catalogs require explicit activation, so a pause before clicking
    // can never replace the entry with an expert underneath the pointer.
    if (width <= 390) {
      await page.waitForTimeout(250);
      assert.equal(await page.getByRole("menu", { name: "专家子菜单" }).count(), 0);
      await experts.click();
    }
    const catalog = page.getByRole("menu", { name: "专家子菜单", exact: true });
    await catalog.waitFor();
    const panel = await catalog.evaluate(el => el.classList.contains("iam-submenu--panel"));
    assert.equal(panel, width <= 390);
    await fits(page, catalog);
    assert.deepEqual(await page.evaluate(() => window.inputMenuReview.calls), [], "opening catalogs must not select an expert");
    if (panel) {
      assert.equal(await main.count(), 0, "parent controls must leave the accessibility tree");
      await page.mouse.move(width - 2, 2);
      await page.waitForTimeout(250);
      assert(await catalog.isVisible(), "panel navigation must survive pointer exit");
      await catalog.getByRole("menuitem", { name: "返回添加菜单" }).click();
      assert(await experts.evaluate(el => el === document.activeElement), "back restores parent focus");
      await page.getByRole("menuitem", { name: "技能", exact: true }).click();
      const skills = page.getByRole("menu", { name: "技能子菜单" });
      await fits(page, skills);
      await skills.getByRole("menuitem", { name: /^文档技能 1 / }).click();
      assert.equal(await page.getByTestId("feedback").innerText(), "skill:skill-1");
      assert.equal(await page.getByRole("menu").count(), 0);
      await page.getByRole("button", { name: "添加", exact: true }).click();
      await page.getByRole("menuitem", { name: "专家", exact: true }).click();
    }
    await catalog.locator(".iam-submenu__scroll").evaluate(el => { el.scrollTop = el.scrollHeight; });
    const last = catalog.getByRole("menuitem", { name: /评审专家 30 / });
    await last.waitFor();
    await last.click({ trial: true });
    await page.screenshot({ path: join(output, `catalog-${width}-${font}-${theme}.png`) });
    await page.keyboard.press("Escape");
    assert(await page.getByRole("button", { name: "添加", exact: true }).evaluate(el => el === document.activeElement));
    await page.keyboard.press("Enter");
    const parent = page.getByRole("menuitem", { name: "专家", exact: true });
    await parent.focus();
    await page.keyboard.press("ArrowRight");
    const first = page.getByRole("menuitem", { name: /评审专家 1 / });
    await first.waitFor();
    await page.waitForFunction(() => document.activeElement?.textContent?.includes("评审专家 1"));
    await page.keyboard.press("ArrowLeft");
    assert(await parent.evaluate(el => el === document.activeElement));
    await page.keyboard.press("ArrowRight");
    await first.waitFor();
    await page.waitForFunction(() => document.activeElement?.textContent?.includes("评审专家 1"));
    await page.keyboard.press("Enter");
    assert.equal(await page.getByTestId("feedback").innerText(), "expert:评审专家 1");
    evidence.push({ width, font, theme, panel });
    await page.close();
  }
  for (const state of ["empty", "error"]) {
    const page = await browser.newPage({ viewport: { width: 350, height: 600 } });
    activePage = page;
    page.on("pageerror", error => errors.push(error.message));
    await page.goto(`${base}?state=${state}`);
    await page.getByRole("button", { name: "添加", exact: true }).click();
    await page.getByRole("menuitem", { name: "专家", exact: true }).click();
    const catalog = page.getByRole("menu", { name: "专家子菜单" });
    await fits(page, catalog);
    if (state === "error") {
      await catalog.getByRole("alert").waitFor();
      await catalog.getByRole("menuitem", { name: "重试" }).click();
      await catalog.getByRole("menuitem", { name: /评审专家 1 / }).waitFor();
      await page.waitForFunction(() => document.activeElement?.textContent?.includes("评审专家 1"));
    } else await catalog.getByText("暂无已安装专家").waitFor();
    await catalog.getByRole("menuitem", { name: "返回添加菜单" }).click();
    assert(await page.getByRole("menuitem", { name: "专家", exact: true }).evaluate(el => el === document.activeElement));
    await page.close();
  }
  const resize = await browser.newPage({ viewport: { width: 1280, height: 800 } });
  activePage = resize;
  resize.on("pageerror", error => errors.push(error.message));
  await resize.goto(base);
  await resize.getByRole("button", { name: "添加", exact: true }).click();
  await resize.getByRole("menuitem", { name: "连接器", exact: true }).focus();
  await resize.keyboard.press("ArrowRight");
  const connectors = resize.getByRole("menu", { name: "连接器子菜单" });
  const firstConnector = connectors.getByRole("menuitem", { name: /^connector-1 / });
  await firstConnector.waitFor();
  await resize.waitForFunction(() => document.activeElement?.textContent?.includes("connector-1"));
  await resize.getByRole("menuitem", { name: "连接器", exact: true }).evaluate(el => el.focus({ preventScroll: true }));
  await resize.setViewportSize({ width: 350, height: 600 });
  await connectors.locator(".iam-submenu__header").waitFor();
  await fits(resize, connectors);
  await resize.waitForFunction(() => document.activeElement?.textContent?.includes("connector-1"));
  await connectors.getByRole("menuitem", { name: "返回添加菜单" }).focus();
  await resize.setViewportSize({ width: 1280, height: 800 });
  await connectors.locator(".iam-submenu__header").waitFor({ state: "detached" });
  await resize.waitForFunction(() => document.activeElement?.textContent?.includes("connector-1"));
  await fits(resize, connectors);
  await firstConnector.click();
  assert.deepEqual(await resize.evaluate(() => window.inputMenuReview.calls), ["connectors"]);
  await resize.close();

  // Focus must scroll the catalog locally, never the chat/composer ancestors.
  const workbenchServer = await createUiReviewServer(0, { freezeMotion: false });
  try {
    for (const [width, font] of [[350, 16], [350, 22], [390, 16], [768, 22], [1280, 16]]) {
      const page = await browser.newPage({ viewport: { width, height: 850 } });
      activePage = page;
      page.on("pageerror", error => errors.push(error.message));
      await page.goto(`http://127.0.0.1:${workbenchServer.httpServer.address().port}/__ui-review?surface=workbench&state=complete&extensionState=long`);
      await page.locator(".echo-composer__add").waitFor();
      if (width < 900) await page.getByRole("button", { name: "收起侧边栏", exact: true }).click();
      await page.evaluate(font => { document.documentElement.style.fontSize = `${font}px`; }, font);
      const composer = page.locator(".echo-composer");
      const before = await composer.boundingBox();
      const assertStable = async () => {
        const after = await composer.boundingBox();
        assert(Math.abs(after.y - before.y) < 1 && Math.abs(after.x - before.x) < 1, `menu focus moved the workbench: ${JSON.stringify({ before, after })}`);
      };
      await pointerClick(page, page.locator(".echo-composer__add"));
      await assertStable();
      const parent = page.getByRole("menuitem", { name: "专家", exact: true });
      if (width <= 390) await pointerClick(page, parent);
      else {
        await parent.evaluate(el => el.focus({ preventScroll: true }));
        await page.keyboard.press("ArrowRight");
      }
      const menu = page.getByRole("menu", { name: "专家子菜单" });
      await menu.waitFor();
      await page.waitForFunction(() => document.activeElement?.closest(".iam-submenu__scroll"));
      await assertStable();
      await page.keyboard.press("End");
      assert(await menu.locator(".iam-submenu__scroll .iam-sub-item").last().evaluate(el => el === document.activeElement));
      assert(await menu.locator(".iam-submenu__scroll").evaluate(el => el.scrollTop > 0));
      await assertStable();
      await page.screenshot({ path: join(output, `workbench-${width}-${font}.png`) });
      await page.keyboard.press("ArrowLeft");
      assert(await parent.evaluate(el => el === document.activeElement));
      const skills = page.getByRole("menuitem", { name: "技能", exact: true });
      await skills.evaluate(el => el.focus({ preventScroll: true }));
      await page.keyboard.press("ArrowRight");
      await page.waitForFunction(() => document.activeElement?.textContent?.includes("需求评审 01"));
      await pointerClick(page, page.getByRole("menu", { name: "技能子菜单" }).getByRole("menuitem").filter({ hasText: "需求评审 01" }));
      assert((await page.locator(".echo-composer__input").inputValue()).includes("/review-skill-0"));
      await assertStable();
      await page.close();
    }
  } finally { await workbenchServer.close(); }
  assert.deepEqual(errors, []);
  writeFileSync(join(output, "evidence.json"), JSON.stringify({ layouts: evidence, workbenchLayouts: 5, flows: ["explicit-panel-entry", "back/focus", "pointer-exit", "scroll-last-item", "keyboard-select", "empty", "retry", "resize/focus", "connector-navigation", "workbench-focus/scroll/skill-insert"], errors }, null, 2));
  console.log(JSON.stringify({ output, layouts: evidence.length, workbenchLayouts: 5, errors }));
} catch (error) {
  if (activePage && !activePage.isClosed()) {
    await activePage.screenshot({ path: join(output, "failure.png") }).catch(() => {});
    console.error(JSON.stringify({ output, menus: (await activePage.getByRole("menu").allTextContents().catch(() => [])).map(text => text.slice(0, 180)), errors }));
  }
  throw error;
} finally {
  await browser?.close();
  await server.close();
}
