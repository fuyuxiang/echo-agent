import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createUiReviewServer, reviewChromium, reviewOutput } from "./ui-review-runtime.mjs";

const output = reviewOutput(mkdtempSync(join(tmpdir(), "echo-settings-navigation-")));
const server = await createUiReviewServer(0);
const origin = `http://127.0.0.1:${server.httpServer.address().port}`;
const layouts = [];
let browser;
try {
  browser = await (await reviewChromium()).launch({ headless: true, executablePath: process.env.CHROME_BIN || "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" });
  for (const [width, height, theme, font] of [[1440, 1000, "light", 13], [1024, 680, "dark", 18], [390, 844, "light", 18], [1280, 580, "dark", 13]]) {
    const page = await browser.newPage({ viewport: { width, height } });
    page.setDefaultTimeout(15000);
    page.setDefaultNavigationTimeout(60000);
    const errors = [];
    page.on("pageerror", error => errors.push(error.message));
    await page.addInitScript(font => localStorage.setItem("echoagent.fontSize", String(font)), font);
    await page.goto(`${origin}/__ui-review?surface=general&theme=${theme}`, { waitUntil: "networkidle" });
    const search = page.getByRole("combobox", { name: "搜索设置" });
    await page.screenshot({ path: join(output, `general-${width}-${theme}.png`) });
    for (const [query, title, target] of [
      ["字号", "字体大小", "font-size"],
      ["后台运行", "关闭窗口后继续运行", "background"],
      ["Webhook", "推送到其他应用", "external-notifications"],
      ["OTLP", "诊断数据上报", "diagnostics"],
      ["数据目录", "本地数据目录", "data-directory"],
      ["常见问题", "常见问题与使用帮助", "help-faq"],
      ["权限规则", "自定义工具规则", "permission-rules"],
    ]) {
      await search.fill(query);
      const results = page.getByRole("listbox", { name: "设置搜索结果" });
      await results.waitFor();
      const box = await results.boundingBox();
      assert(box && box.x >= 0 && box.x + box.width <= width + 1 && box.y + box.height <= height + 1, `${width}: search results clipped ${JSON.stringify(box)}`);
      await results.getByRole("option").filter({ hasText: title }).click();
      const setting = page.locator(`[data-setting-id="${target}"]`);
      await setting.waitFor({ state: "visible" });
      if (await setting.evaluate(element => element.tagName === "DETAILS")) assert(await setting.evaluate(element => element.open), `${title}: details did not expand`);
      if (target === "help-faq") {
        assert(await page.getByText("确认模型配置", { exact: true }).isVisible(), "FAQ search left its content hidden");
        await page.waitForFunction(() => document.activeElement === document.querySelector('[data-setting-id="help-faq"] > summary'));
      }
      if (target === "external-notifications") assert.equal(await setting.getByRole("button", { name: /推送到其他应用/ }).getAttribute("aria-expanded"), "true");
      const overflow = await page.locator(".settings-modal__panel").evaluate(element => element.scrollWidth > element.clientWidth + 1);
      assert(!overflow, `${width}: ${title} overflows settings`);
      layouts.push({ width, height, theme, font, query, title });
    }

    // Exercise actual keyboard navigation and the outer modal's Escape trap.
    await search.fill("模型");
    const beforeComposition = await page.locator(".settings-modal__location strong").textContent();
    const beforeSelection = await search.getAttribute("aria-activedescendant");
    const compositionKeys = await search.evaluate(input => {
      const prevented = [];
      for (const props of [{ isComposing: true }, { isComposing: false, keyCode: 229 }]) {
        for (const key of ["ArrowDown", "ArrowUp", "Enter", "Escape"]) {
          const event = new KeyboardEvent("keydown", { ...props, key, bubbles: true, cancelable: true });
          input.dispatchEvent(event);
          prevented.push(event.defaultPrevented);
        }
      }
      // WebView composition events remain authoritative when the key event
      // itself lacks an isComposing flag.
      input.dispatchEvent(new CompositionEvent("compositionstart", { bubbles: true }));
      input.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true, cancelable: true }));
      input.dispatchEvent(new CompositionEvent("compositionend", { bubbles: true }));
      return prevented;
    });
    assert(compositionKeys.every(prevented => !prevented), "Search intercepted input method keys");
    assert.equal(await search.inputValue(), "模型", "Input method confirmation cleared the query");
    assert.equal(await search.getAttribute("aria-activedescendant"), beforeSelection, "Input method keys moved the search selection");
    assert.equal(await page.locator(".settings-modal__location strong").textContent(), beforeComposition, "Input method confirmation navigated Settings");
    await search.press("Tab");
    assert(await page.getByRole("button", { name: "关闭设置", exact: true }).evaluate(element => document.activeElement === element), "Search options should use arrow keys without adding Tab stops");
    await search.focus();
    await search.press("Escape");
    assert.equal(await search.getAttribute("aria-expanded"), "false");
    assert(await page.getByRole("dialog", { name: "设置", exact: true }).isVisible());
    await search.fill("模型");
    await search.press("ArrowDown");
    const active = await search.getAttribute("aria-activedescendant");
    assert(active && await page.locator(`[id="${active}"]`).isVisible(), "Keyboard selection is not visible");
    assert.equal(await page.locator(`[id="${active}"] > span`).textContent(), "模型用量", "ArrowDown selected an unexpected result");
    await search.press("Enter");
    await page.getByRole("heading", { name: "模型用量", exact: true }).waitFor();

    // Pending input survives a cancelled search navigation and a cancelled
    // close. Saving the list removes the guard, and the next visit reads it.
    await search.fill("权限规则");
    await page.getByRole("option").filter({ hasText: "自定义工具规则" }).click();
    const pattern = page.getByPlaceholder("例如 git *");
    await pattern.fill("git status");
    await search.fill("备份");
    await page.getByRole("option").filter({ hasText: "数据备份与恢复" }).click();
    const confirmation = page.getByRole("alertdialog", { name: "舍弃未保存的修改？" });
    await confirmation.getByRole("button", { name: "取消", exact: true }).click();
    assert.equal(await pattern.inputValue(), "git status");
    assert(await search.evaluate(element => document.activeElement === element), "Cancelling search navigation lost keyboard focus");
    await page.getByRole("button", { name: "关闭设置", exact: true }).click();
    await confirmation.getByRole("button", { name: "取消", exact: true }).click();
    await page.getByRole("button", { name: "添加到列表", exact: true }).click();
    await page.evaluate(() => {
      const invoke = window.__TAURI_INTERNALS__.invoke;
      window.__TAURI_INTERNALS__.invoke = async (command, args) => {
        if (command === "permission_save") await new Promise(resolve => { window.releaseSettingsSave = resolve; });
        return invoke(command, args);
      };
    });
    await page.getByRole("button", { name: "保存全部规则", exact: true }).click();
    await page.getByRole("button", { name: "关闭设置", exact: true }).click();
    const busyFeedback = page.getByRole("status").filter({ hasText: "正在处理，请等待完成" });
    await busyFeedback.waitFor();
    assert(await page.getByRole("heading", { name: "安全与权限", exact: true }).isVisible());
    await page.evaluate(() => window.releaseSettingsSave());
    await page.getByRole("status").filter({ hasText: "规则已保存" }).waitFor();
    await busyFeedback.waitFor({ state: "hidden" });
    await search.fill("备份");
    await page.getByRole("option").filter({ hasText: "数据备份与恢复" }).click();
    await page.getByRole("button", { name: "从备份恢复", exact: true }).waitFor();
    assert.equal(await page.getByRole("alertdialog").count(), 0);
    await page.screenshot({ path: join(output, `data-${width}-${theme}.png`) });
    assert.deepEqual(errors, []);
    console.log(`Settings navigation, search and drafts passed at ${width}×${height}, ${theme}, font ${font}`);
    await page.close();
  }
  writeFileSync(join(output, "navigation.json"), JSON.stringify(layouts, null, 2));
  console.log(`Passed ${layouts.length} search destinations plus keyboard and draft workflows; evidence: ${output}`);
} finally { await browser?.close(); await server.close(); }
