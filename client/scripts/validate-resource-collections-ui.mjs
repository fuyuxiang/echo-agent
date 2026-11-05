import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createUiReviewServer, reviewChromium, reviewOutput } from "./ui-review-runtime.mjs";
import { validateExtensionLayouts } from "./validate-extension-layout.mjs";

const output = reviewOutput(mkdtempSync(join(tmpdir(), "echo-resource-collections-")));
const server = await createUiReviewServer(1439);
const base = "http://127.0.0.1:1439/__ui-review";
const evidence = [];
const errors = [];
let browser;
try {
  browser = await (await reviewChromium()).launch({ headless: true, executablePath: process.env.CHROME_BIN || "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" });
  const page = await browser.newPage();
  page.on("pageerror", error => errors.push(error.message));
  // Only clipped descriptions need an expander; it must work with a keyboard
  // and keep both the full text and controls usable as the available width changes.
  for (const theme of ["light", "dark"]) {
    await page.setViewportSize({ width: 1280, height: 900 });
    await page.goto(`${base}?surface=capabilities&label=${encodeURIComponent("插件")}&extensionState=populated&pluginDescriptions=1&theme=${theme}`);
    const rows = page.locator(".resource-list > .resource-row");
    const long = rows.nth(0), short = rows.nth(1), responsive = rows.nth(2);
    const expand = long.getByRole("button", { name: "展开插件「需求评审 01」的说明", exact: true });
    await expand.waitFor();
    assert.equal(await short.locator(".resource-description__toggle").count(), 0, "short descriptions need no extra control");
    assert.equal(await responsive.locator(".resource-description__toggle").count(), 0, "wide preview fits without an expander");
    const text = long.locator(".resource-description__text");
    const collapsedHeight = await text.evaluate(element => element.clientHeight);
    assert(await text.evaluate(element => element.scrollHeight > element.clientHeight + 1), "long preview is initially compact");
    assert.equal(await expand.getAttribute("aria-controls"), await text.getAttribute("id"));
    await expand.focus();
    await page.keyboard.press("Space");
    const collapse = long.getByRole("button", { name: "收起插件「需求评审 01」的说明", exact: true });
    assert.equal(await collapse.getAttribute("aria-expanded"), "true");
    assert(await text.evaluate(element => element.clientHeight >= element.scrollHeight - 1), "expanded description exposes every line");
    assert((await text.evaluate(element => element.clientHeight)) > collapsedHeight);
    assert(await long.evaluate(row => {
      const top = row.getBoundingClientRect().top;
      return row.querySelector(".resource-icon").getBoundingClientRect().top < top + 40
        && row.querySelector(".resource-row__actions").getBoundingClientRect().top < top + 40;
    }), "long descriptions keep the icon and actions beside the heading");
    await page.screenshot({ path: join(output, `plugin-description-expanded-wide-${theme}.png`) });
    await page.getByRole("button", { name: "刷新", exact: true }).click();
    assert.equal(await collapse.getAttribute("aria-expanded"), "true", "refreshing unchanged text preserves reading position");
    await collapse.focus();
    await page.keyboard.press("Enter");
    assert.equal(await expand.getAttribute("aria-expanded"), "false");
    assert(await expand.evaluate(element => element === document.activeElement), "collapse retains keyboard focus");

    await page.setViewportSize({ width: 350, height: 900 });
    await page.evaluate(() => { document.documentElement.style.fontSize = "22px"; });
    const responsiveToggle = responsive.locator(".resource-description__toggle");
    await responsiveToggle.waitFor();
    assert.equal(await short.locator(".resource-description__toggle").count(), 0);
    await expand.click();
    assert(await text.evaluate(element => element.clientHeight >= element.scrollHeight - 1), "narrow large-text description is fully readable");
    assert(await long.evaluate(element => element.scrollWidth <= element.clientWidth + 1), "expanded text does not overflow its row");
    assert(await page.evaluate(() => document.querySelector(".capability-page").scrollWidth <= document.querySelector(".capability-page").clientWidth + 1), "expanded description stays inside the page");
    await page.screenshot({ path: join(output, `plugin-description-expanded-${theme}.png`) });
    await responsiveToggle.click();
    await page.setViewportSize({ width: 1280, height: 900 });
    await page.evaluate(() => { document.documentElement.style.fontSize = "16px"; });
    // Let ResizeObserver process the new line height and available width.
    await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
    await responsiveToggle.focus();
    await page.keyboard.press("Enter");
    assert.equal(await responsiveToggle.count(), 0, "expander becomes unnecessary when the entire description fits");
    assert(await responsive.locator(".resource-description__text").evaluate(element => element === document.activeElement), "removing an unnecessary toggle keeps focus at its text");
  }
  const extensionLayouts = await validateExtensionLayouts(page, output);
  for (const [width, font, theme] of [[1280, 16, "light"], [1280, 16, "dark"], [1024, 22, "dark"], [768, 22, "light"], [390, 16, "light"], [350, 22, "dark"]]) {
    await page.setViewportSize({ width, height: 900 });
    for (const label of ["专家·技能·连接器", "技能", "连接器", "插件"]) {
      await page.goto(`${base}?surface=capabilities&label=${encodeURIComponent(label)}&extensionState=populated&theme=${theme}`);
      await page.locator(".resource-list .resource-row").first().waitFor();
      await page.evaluate(font => { document.documentElement.style.fontSize = `${font}px`; }, font);
      const geometry = await page.locator(".resource-list").first().evaluate(list => {
        const bounds = list.getBoundingClientRect();
        const rows = [...list.querySelectorAll(":scope > .resource-row")].map(row => {
          const box = row.getBoundingClientRect();
          const styles = getComputedStyle(row);
          const actions = row.querySelector(".resource-row__actions");
          const actionBox = actions?.getBoundingClientRect();
          const main = row.querySelector(".resource-row__main,.plugins-panel__item-content,.mcp-item-main");
          const mainBox = main?.getBoundingClientRect();
          const icon = row.querySelector(".resource-icon,.colleague-avatar");
          return {
            left: box.left, right: box.right, top: box.top, bottom: box.bottom,
            padding: styles.padding, shadow: styles.boxShadow,
            overflow: row.scrollWidth > row.clientWidth + 1,
            iconWidth: icon?.getBoundingClientRect().width,
            actionsFit: !actionBox || actionBox.left >= box.left && actionBox.right <= box.right + 1,
            actionsOverlap: !!(actionBox && mainBox && actionBox.width && mainBox.right > actionBox.left + 1 && mainBox.bottom > actionBox.top + 1 && mainBox.top < actionBox.bottom - 1),
          };
        });
        return { left: bounds.left, right: bounds.right, overflow: document.documentElement.scrollWidth > innerWidth + 1 || list.scrollWidth > list.clientWidth + 1, rows };
      });
      assert(!geometry.overflow, `${label}/${width}/${font}: collection overflow`);
      assert.equal(geometry.rows.length, 3);
      for (const [index, row] of geometry.rows.entries()) {
        assert(!row.overflow && row.actionsFit && !row.actionsOverlap, `${label}/${width}/${font}: controls overlap or overflow: ${JSON.stringify(row)}`);
        assert.equal(row.shadow, "none", `${label}: static rows have no card shadows`);
        assert(Math.abs(row.left - geometry.left) <= 1 && Math.abs(row.right - geometry.right) <= 1, `${label}: rows fill the list width`);
        if (index) assert(row.top >= geometry.rows[index - 1].bottom - 1, `${label}: resources form one vertical list`);
      }
      evidence.push({ label, width, font, theme, ...geometry });
      await page.screenshot({ path: join(output, `resources-${label}-${width}-${font}-${theme}.png`) });
    }
  }
  // Expert, project and automation templates use the production TemplateCard.
  const templates = [];
  for (const surface of ["management-experts", "projects", "automation-templates"]) {
    await page.setViewportSize({ width: 1280, height: 900 });
    await page.goto(`${base}?surface=${surface}&collapsed=1`);
    await page.locator(".resource-template").first().waitFor();
    const styles = await page.locator(".resource-template").first().evaluate(card => {
      const s = getComputedStyle(card);
      return { padding: s.padding, radius: s.borderRadius, shadow: s.boxShadow, title: getComputedStyle(card.querySelector(".resource-template__title")).fontSize, icon: card.querySelector(".resource-icon").getBoundingClientRect().width };
    });
    templates.push({ surface, ...styles });
    await page.screenshot({ path: join(output, `templates-${surface}.png`) });
    for (const width of [390, 768]) {
      await page.setViewportSize({ width, height: 900 });
      await page.evaluate(() => { document.documentElement.style.fontSize = "22px"; });
      assert(await page.locator(".resource-template").evaluateAll(cards => cards.every(card => card.scrollWidth <= card.clientWidth + 1)), `${surface}/${width}: template overflow with large text`);
    }
  }
  for (const template of templates.slice(1)) {
    const { surface, ...style } = template;
    const { surface: firstSurface, ...reference } = templates[0];
    assert.deepEqual(style, reference, `${surface} and ${firstSurface} share template presentation`);
  }
  assert.deepEqual(errors, []);
  writeFileSync(join(output, "layouts.json"), JSON.stringify({ extensionLayouts, resources: evidence, templates }, null, 2));
  console.log(JSON.stringify({ output, extensionLayouts, collectionLayouts: evidence.length, templateTypes: templates.length, errors }));
} finally {
  await browser?.close();
  await server.close();
}
