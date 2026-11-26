import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createUiReviewServer, reviewChromium, reviewOutput } from "./ui-review-runtime.mjs";

const output = reviewOutput(mkdtempSync(join(tmpdir(), "echo-model-settings-")));
const server = await createUiReviewServer(0);
const origin = `http://127.0.0.1:${server.httpServer.address().port}/__ui-review?surface=model&modelSettingsReview=1`;
const layouts = [];
let browser;

async function capture(page, name) {
  for (const selector of [".settings-modal__panel", ".model-connections", ".provider-card", ".model-connection-editor", ".model-connection-editor__body"]) {
    const overflow = await page.locator(selector).evaluateAll(elements => elements.some(el => el.scrollWidth > el.clientWidth + 1));
    assert.equal(overflow, false, `${name}: ${selector} overflows`);
  }
  const editor = page.locator(".model-connection-editor");
  if (await editor.count()) {
    const geometry = await editor.evaluate(dialog => {
      const body = dialog.querySelector(".model-connection-editor__body");
      const header = dialog.querySelector("header");
      const footer = dialog.querySelector("footer");
      const originalScroll = body.scrollTop;
      const footerTop = footer.getBoundingClientRect().top;
      body.scrollTop = body.scrollHeight;
      const scrolls = body.scrollHeight > body.clientHeight + 1;
      const bodyScrolled = body.scrollTop > 0;
      body.scrollTop = originalScroll;
      return {
        separated: header.getBoundingClientRect().bottom <= body.getBoundingClientRect().top + 1
          && body.getBoundingClientRect().bottom <= footerTop + 1,
        footerVisible: footer.getBoundingClientRect().bottom <= innerHeight + 1,
        footerFixed: footer.getBoundingClientRect().top === footerTop,
        scrollsWhenNeeded: !scrolls || bodyScrolled,
      };
    });
    assert(Object.values(geometry).every(Boolean), `${name}: ${JSON.stringify(geometry)}`);
  }
  await page.screenshot({ path: join(output, `${name}.png`) });
  layouts.push(name);
}

try {
  browser = await (await reviewChromium()).launch({ headless: true, executablePath: process.env.CHROME_BIN || "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" });
  for (const [width, height, theme, font] of [[1440, 1000, "light", 13], [1024, 620, "dark", 18], [768, 900, "light", 18], [390, 640, "dark", 18]]) {
    const page = await browser.newPage({ viewport: { width, height } });
    page.setDefaultTimeout(15000);
    page.setDefaultNavigationTimeout(60000);
    const errors = [];
    page.on("pageerror", error => errors.push(error.message));
    await page.addInitScript(font => {
      localStorage.setItem("echoagent.fontSize", String(font));
      const observer = new MutationObserver(() => {
        if (document.documentElement) { document.documentElement.style.fontSize = `${16 * font / 13}px`; observer.disconnect(); }
      });
      observer.observe(document, { childList: true, subtree: true });
    }, font);
    await page.goto(`${origin}&theme=${theme}`, { waitUntil: "networkidle" });
    const catalog = page.getByRole("region", { name: "模型目录", exact: true });
    const personal = catalog.getByRole("region", { name: "工作模型服务", exact: true });
    await personal.getByRole("button", { name: "测试模型 工作模型", exact: true }).waitFor();
    assert.equal(await catalog.getByRole("searchbox").count(), 0, "Short lists should stay compact");
    assert.equal(await catalog.getByRole("combobox", { name: "默认模型", exact: true }).count(), 0);
    assert.equal(await page.getByRole("button", { name: "管理服务", exact: true }).count(), 0);
    assert.equal(await personal.getByText("工具调用 · 图片输入", { exact: true }).count(), 0);
    await page.evaluate(() => document.activeElement?.blur());
    await capture(page, `model-page-${width}-${theme}`);

    const open = catalog.getByRole("button", { name: "添加模型", exact: true });
    await open.click();
    const add = page.getByRole("dialog", { name: "添加模型", exact: true });
    const provider = add.getByLabel("供应商", { exact: true });
    assert(await provider.evaluate(el => el === document.activeElement));
    await provider.selectOption("kimi");
    const key = add.getByLabel("API Key", { exact: true });
    const modelName = add.getByLabel("模型名称", { exact: true });
    const action = add.getByRole("button", { name: "测试连接", exact: true });
    assert(await key.isVisible() && await modelName.isVisible());
    assert.equal(await add.locator("#model-base-url").isVisible(), false);
    await page.evaluate(() => document.activeElement?.blur());
    await capture(page, `add-preset-${width}-${theme}`);
    await action.click();
    assert.equal(await key.getAttribute("aria-invalid"), "true");
    await key.fill("isolated-review-key");
    await action.click();
    await add.getByRole("radio", { name: "new-model", exact: true }).check();
    assert.equal(await modelName.inputValue(), "new-model");
    await provider.selectOption("custom");
    assert.equal(await key.inputValue(), "", "Switching origin clears the previous credential");
    assert.equal(await modelName.inputValue(), "new-model");
    assert.equal(await add.getByRole("radio").count(), 0);
    const address = add.getByLabel("接口地址", { exact: true });
    const [providerBox, addressBox, keyBox, modelBox, actionBox] = await Promise.all([provider, address, key, modelName, action].map(field => field.boundingBox()));
    assert(providerBox.y < addressBox.y && addressBox.y < keyBox.y && keyBox.y < modelBox.y);
    assert(Math.abs(actionBox.y - keyBox.y) <= 2 && actionBox.x > keyBox.x);
    await modelName.fill("");
    await page.evaluate(() => document.activeElement?.blur());
    await add.locator(".model-connection-editor__body").evaluate(body => { body.scrollTop = 0; });
    await capture(page, `add-custom-${width}-${theme}`);
    await address.fill("https://isolated-review.test/v1");
    await key.fill("isolated-review-key");
    await add.getByRole("button", { name: "保存模型", exact: true }).click();
    assert.equal(await modelName.getAttribute("aria-invalid"), "true");
    await modelName.fill("review-custom");
    await add.getByRole("checkbox", { name: "工具调用", exact: true }).check();
    const context = add.getByRole("group", { name: "常用上下文窗口" });
    await context.getByRole("button", { name: "64K", exact: true }).click();
    await capture(page, `add-custom-options-${width}-${theme}`);
    await add.getByRole("button", { name: "保存模型", exact: true }).click();
    const customGroup = catalog.getByRole("region", { name: "OpenAI 兼容", exact: true });
    await customGroup.getByRole("button", { name: "测试模型 review-custom", exact: true }).waitFor();
    assert.doesNotMatch(await customGroup.textContent(), /isolated-review\.test/);
    assert.equal(await page.evaluate(() => window.modelSettingsReviewCalls.defaults), 0);

    const editModel = personal.getByRole("button", { name: "编辑模型 工作模型", exact: true });
    if (await editModel.isVisible()) await editModel.click();
    else {
      await personal.getByRole("button", { name: "模型操作 工作模型", exact: true }).click();
      await page.getByRole("menuitem", { name: "编辑", exact: true }).click();
    }
    const edit = page.getByRole("dialog", { name: "编辑模型", exact: true });
    const keyField = edit.getByLabel("API Key", { exact: true });
    assert.equal(await keyField.inputValue(), "");
    const shared = edit.getByRole("note");
    assert.match(await shared.textContent(), /此连接由 2 个模型共用.*API Key.*同时影响/);
    assert.equal(await keyField.getAttribute("aria-describedby"), await shared.getAttribute("id"));
    assert.equal(await edit.getByLabel("接口地址", { exact: true }).getAttribute("aria-describedby"), await shared.getAttribute("id"));
    assert.equal(await edit.getByLabel("模型名称", { exact: true }).getAttribute("aria-describedby"), null);
    assert(await edit.getByRole("checkbox", { name: "图片输入", exact: true }).isChecked());
    await capture(page, `edit-model-${width}-${theme}`);
    await edit.getByRole("button", { name: "测试连接", exact: true }).click();
    await edit.getByRole("status").filter({ hasText: "测试通过，模型已成功回复。" }).waitFor();
    await edit.getByRole("group", { name: "常用最大输出" }).getByRole("button", { name: "8K", exact: true }).click();
    assert.equal(await edit.getByLabel("输出，最大输出（tokens）", { exact: true }).inputValue(), "8192");
    await edit.getByRole("button", { name: "保存模型", exact: true }).click();
    await page.getByRole("status").filter({ hasText: "模型已更新。" }).waitFor();

    await open.click();
    await provider.selectOption("saved:review-personal");
    assert.equal(await add.getByLabel("API Key", { exact: true }).count(), 0);
    await add.getByRole("button", { name: "从服务获取模型", exact: true }).click();
    await add.getByRole("radio", { name: "new-model", exact: true }).check();
    assert.equal(await modelName.inputValue(), "new-model");
    await add.getByRole("button", { name: "保存模型", exact: true }).click();
    await personal.getByRole("button", { name: "测试模型 new-model", exact: true }).waitFor();

    if (width === 1440 || width === 390) {
      await open.click();
      await provider.selectOption("minimax");
      await modelName.fill("MiniMax-M3.1-Flash");
      await add.getByText("高级配置", { exact: true }).click();
      const thinking = add.getByRole("checkbox", { name: "调节思考强度", exact: true });
      const effort = add.getByLabel("默认思考强度", { exact: true });
      assert(await add.getByRole("checkbox", { name: "工具调用", exact: true }).isChecked());
      assert(await add.getByRole("checkbox", { name: "图片输入", exact: true }).isChecked());
      assert(await thinking.isChecked());
      await effort.scrollIntoViewIfNeeded();
      assert.equal(await effort.inputValue(), "");
      assert.deepEqual(await effort.locator("option").evaluateAll(options => options.map(option => option.value)), ["", "low", "medium", "high", "xhigh", "max"]);
      await capture(page, `reasoning-on-${width}-${theme}`);
      await thinking.uncheck();
      assert.equal(await effort.count(), 0);
      await capture(page, `reasoning-off-${width}-${theme}`);
      await add.getByRole("button", { name: "取消", exact: true }).click();
      await page.getByRole("button", { name: "舍弃修改", exact: true }).click();
    }

    await page.goto(`${origin}&modelLongCatalog=1&initialModelId=review-personal/main&theme=${theme}`, { waitUntil: "networkidle" });
    await page.waitForFunction(() => document.activeElement?.getAttribute("data-model-id") === "review-personal/main");
    const search = catalog.getByRole("searchbox", { name: "搜索模型", exact: true });
    await search.fill("backup-model");
    assert.equal(await personal.getByRole("listitem").count(), 1);
    await search.clear();
    const last = catalog.getByRole("button", { name: "模型操作 长目录模型 24", exact: true });
    await last.scrollIntoViewIfNeeded();
    assert(await last.isEnabled());
    await capture(page, `long-catalog-${width}-${theme}`);
    assert.deepEqual(errors, []);
    console.log(`Model settings passed at ${width}×${height}, ${theme}, font ${font}`);
    await page.close();
  }

  const page = await browser.newPage({ viewport: { width: 1280, height: 850 } });
  page.setDefaultNavigationTimeout(60000);
  for (const account of ["none", "mixed", "expired", "org-empty"]) {
    await page.goto(`${origin}&modelAccount=${account}`, { waitUntil: "networkidle" });
    const catalog = page.getByRole("region", { name: "模型目录", exact: true });
    await catalog.waitFor();
    await capture(page, `account-${account}`);
    if (account === "none") {
      await catalog.getByText("还没有可用模型").waitFor();
      await page.getByRole("button", { name: "添加模型", exact: true }).click();
      const dialog = page.getByRole("dialog", { name: "添加模型", exact: true });
      await dialog.getByLabel("供应商", { exact: true }).selectOption("deepseek");
      await dialog.getByLabel("API Key", { exact: true }).fill("isolated-review-key");
      await dialog.getByLabel("模型名称", { exact: true }).fill("review-first-model");
      await dialog.getByRole("button", { name: "保存模型", exact: true }).click();
      await page.getByRole("region", { name: "DeepSeek", exact: true }).getByText("review-first-model", { exact: true }).waitFor();
      await page.getByRole("button", { name: "编辑模型 review-first-model", exact: true }).click();
      const editor = page.getByRole("dialog", { name: "编辑模型", exact: true });
      assert.equal(await editor.getByRole("note").count(), 0);
      await editor.getByRole("button", { name: "取消", exact: true }).click();
      await page.getByRole("button", { name: "高级设置", exact: true }).click();
      await page.getByRole("heading", { name: "高级设置", exact: true }).waitFor();
      assert.equal(await page.getByRole("button", { name: "管理服务", exact: true }).count(), 0);
      assert.equal(await page.getByRole("region", { name: "服务连接", exact: true }).count(), 0);
    }
    if (account === "mixed") {
      const team = catalog.getByRole("region", { name: "团队模型服务", exact: true });
      assert.equal(await team.getByRole("button", { name: "编辑模型 团队模型", exact: true }).count(), 0);
    }
    if (account === "expired") {
      await page.getByRole("button", { name: "前往组织登录", exact: true }).click();
      assert.equal(await page.evaluate(() => window.modelSettingsReviewCalls.logins), 1);
    }
    if (account === "org-empty") {
      await page.getByRole("button", { name: "刷新组织模型", exact: true }).click();
      await page.getByRole("status", { name: "组织模型状态" }).filter({ hasText: "组织暂未提供模型" }).waitFor();
    }
  }
  await page.goto(`${origin}&modelReadError=1`, { waitUntil: "networkidle" });
  await page.getByRole("alert").filter({ hasText: "配置暂不可读" }).waitFor();
  await page.getByRole("button", { name: "重新读取", exact: true }).click();
  await page.getByRole("region", { name: "模型目录", exact: true }).waitFor();
  await capture(page, "read-recovery");
  writeFileSync(join(output, "evidence.json"), JSON.stringify({ layouts }, null, 2));
  console.log(`Passed ${layouts.length} model settings layouts and workflows. Evidence: ${output}`);
} finally {
  await browser?.close();
  await server.close();
}
