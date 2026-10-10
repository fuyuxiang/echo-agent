import assert from "node:assert/strict";
import { join } from "node:path";
import { writeFileSync } from "node:fs";
import { createUiReviewServer, reviewChromium, reviewOutput } from "./ui-review-runtime.mjs";
import { validateResultReview } from "./validate-result-review.mjs";
import { validateExecutionReview } from "./validate-execution-review.mjs";

const output = reviewOutput("/private/tmp/echo-ui-review-regression");
const server = await createUiReviewServer();
const browser = await (await reviewChromium()).launch({ headless: true, executablePath: process.env.CHROME_BIN || "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" });
const page = await browser.newPage();
page.setDefaultNavigationTimeout(60000);
const errors = [];
const checks = [];
const baseline = process.env.UI_REVIEW_BASELINE === "1";
page.on("pageerror", error => errors.push(error.message));
async function inspect(name, selector, header, footer, body) {
  const geometry = await page.evaluate(({ selector, header, footer, body }) => {
    const rect = el => { const { x, y, width, height, bottom, right } = el.getBoundingClientRect(); return { x, y, width, height, bottom, right }; };
    const container = document.querySelector(selector);
    const scroller = body && container.querySelector(body);
    return { dialog: rect(container), header: header && container.querySelector(header) ? rect(container.querySelector(header)) : null, footer: footer && container.querySelector(footer) ? rect(container.querySelector(footer)) : null, body: scroller ? rect(scroller) : null, scroll: scroller ? { top: scroller.scrollTop, height: scroller.scrollHeight, clientHeight: scroller.clientHeight } : null, horizontalOverflow: container.scrollWidth > container.clientWidth + 1 };
  }, { selector, header, footer, body });
  if (!baseline) {
    const { dialog, header: top, footer: actions, body: content } = geometry;
    const { width, height } = page.viewportSize();
    assert.ok(dialog.x >= 0 && dialog.y >= 0 && dialog.right <= width + 1 && dialog.bottom <= height + 1, `${name}: dialog clips viewport`);
    assert.ok(!geometry.horizontalOverflow, `${name}: dialog horizontal overflow`);
    if (top && content) assert.ok(top.bottom <= content.y + 1, `${name}: body overlaps title`);
    if (actions && content) assert.ok(content.bottom <= actions.y + 1 && actions.bottom <= dialog.bottom + 1, `${name}: actions overlap or clip`);
    if (name.includes("-scrolled-") && geometry.scroll && geometry.scroll.height > geometry.scroll.clientHeight + 1) assert.ok(geometry.scroll.top > 0, `${name}: content cannot actually scroll`);
  }
  checks.push({ name, ...geometry });
  await page.screenshot({ path: join(output, `${name}.png`) });
}
try {
  for (const [width, height] of [[1024, 680], [1280, 800], [1920, 1080]]) for (const theme of ["light", "dark"]) {
    await page.setViewportSize({ width, height });
    for (const surface of ["modal-short", "modal-long", "modal-error"]) {
      await page.goto(`http://127.0.0.1:1439/__ui-review?surface=${surface}&theme=${theme}`);
      await page.locator(".app-dialog").waitFor();
      if (surface === "modal-error") await page.getByRole("button", { name: "保存说明", exact: true }).click();
      await inspect(`${surface}-${width}-${theme}`, ".app-dialog", ".app-dialog__header", ".app-dialog__actions", ".app-dialog__body");
      if (!baseline) {
        if (surface === "modal-short") assert.ok((await page.locator(".app-dialog").boundingBox()).height < 280, "short confirmation is oversized");
        else {
          await page.locator(".app-dialog__body").evaluate(el => { el.scrollTop = el.scrollHeight; });
          await inspect(`${surface}-scrolled-${width}-${theme}`, ".app-dialog", ".app-dialog__header", ".app-dialog__actions", ".app-dialog__body");
          if (surface === "modal-error") await page.getByRole("alert").waitFor();
        }
        await page.keyboard.press("Escape");
        await page.locator(".app-dialog").waitFor({ state: "hidden" });
      }
    }
    for (const variant of ["expert", "connector", "token", "upload"]) {
      await page.goto(`http://127.0.0.1:1439/__ui-review?surface=details&detailVariant=${variant}&theme=${theme}`);
      const isUpload = variant === "upload";
      await page.locator(isUpload ? ".sk-upload" : ".ec-modal").waitFor();
      await inspect(`details-${variant}-${width}-${theme}`, isUpload ? ".sk-upload" : ".ec-modal", isUpload ? ".sk-import-head" : ".ec-modal-header", isUpload ? ".sk-upload-footer" : ".ec-modal-footer", isUpload ? ".sk-import-body" : ".ec-modal-body");
      if (!baseline) {
        await page.locator(isUpload ? ".sk-import-body" : ".ec-modal-body").evaluate(el => { el.scrollTop = el.scrollHeight; });
        await inspect(`details-${variant}-scrolled-${width}-${theme}`, isUpload ? ".sk-upload" : ".ec-modal", isUpload ? ".sk-import-head" : ".ec-modal-header", isUpload ? ".sk-upload-footer" : ".ec-modal-footer", isUpload ? ".sk-import-body" : ".ec-modal-body");
        if (variant === "token") {
          await page.locator(".cn-token-input").first().fill("隔离测试输入");
          await page.keyboard.press("Escape");
          await page.getByRole("alertdialog", { name: "舍弃未保存的修改？" }).waitFor();
          await page.getByRole("button", { name: "继续编辑" }).click();
          assert.equal(await page.locator(".cn-token-input").first().inputValue(), "隔离测试输入");
        }
      }
    }
    for (const state of ["short", "long", "loading", "error"]) {
      await page.goto(`http://127.0.0.1:1439/__ui-review?surface=trust&state=${state}&theme=${theme}`);
      await page.locator(".trust-dialog").waitFor();
      await page.getByRole(state === "error" ? "alert" : state === "loading" ? "status" : "button", state === "short" || state === "long" ? { name: "不信任", exact: true } : {}).waitFor();
      await inspect(`trust-${state}-${width}-${theme}`, ".trust-dialog", ".trust-dialog__header", ".trust-dialog__footer", ".trust-dialog__body");
      if (!baseline) {
        if (state === "short") {
          assert.ok((await page.locator(".trust-dialog").boundingBox()).height < 450, "short trust dialog is oversized");
          await page.getByRole("button", { name: "不信任", exact: true }).click();
          await page.getByRole("status").waitFor();
          await page.locator(".trust-dialog").waitFor({ state: "hidden" });
        } else if (state === "long" || state === "error") {
          await page.locator(".trust-dialog__body").evaluate(el => { el.scrollTop = el.scrollHeight; });
          await inspect(`trust-${state}-scrolled-${width}-${theme}`, ".trust-dialog", ".trust-dialog__header", ".trust-dialog__footer", ".trust-dialog__body");
          assert.ok(await page.getByRole("button", { name: state === "long" ? "不信任" : "重试", exact: true }).isVisible());
        }
      }
    }
    for (const form of ["project", "expert", "model"]) {
      const surface = form === "project" ? "page&label=项目" : form === "expert" ? "capabilities" : "model";
      await page.goto(`http://127.0.0.1:1439/__ui-review?surface=${surface}&theme=${theme}`);
      await page.getByRole("button", { name: form === "project" ? "新建项目" : form === "expert" ? "创建专家" : "添加个人连接", exact: true }).first().click();
      const shell = form === "model" ? ".model-connection-editor" : ".create-colleague-dialog";
      await page.locator(shell).waitFor();
      const header = form === "model" ? ".models-settings-panel__editor-header" : ".create-colleague-header";
      const body = form === "model" ? ".model-connection-editor__body" : ".create-colleague-body";
      const footer = form === "model" ? ".models-settings-panel__editor-footer" : ".create-colleague-footer";
      if (form === "model") await page.getByRole("button", { name: "高级设置", exact: true }).click();
      if (form !== "model") await page.locator(`${shell} textarea`).first().fill("项目约束、结果要求与操作说明。".repeat(45));
      await inspect(`form-${form}-${width}-${theme}`, shell, header, footer, body);
      if (form === "project") {
        await page.locator(".proj-tpl-select__btn").click();
        await page.locator(".proj-tpl-select__menu").waitFor();
        await page.screenshot({ path: join(output, `project-template-menu-${width}-${theme}.png`) });
        if (!baseline) {
          const box = await page.locator(".proj-tpl-select__menu").boundingBox();
          assert.ok(box.x >= 0 && box.y >= 0 && box.x + box.width <= width && box.y + box.height <= height, "project template menu clips viewport");
          await page.keyboard.press("Escape");
          await page.locator(".proj-tpl-select__menu").waitFor({ state: "hidden" });
          assert.ok(await page.locator(shell).isVisible(), "template Escape closed the project editor");
        }
      }
      if (!baseline) {
        await page.locator(body).evaluate(el => { el.scrollTop = el.scrollHeight; });
        await inspect(`form-${form}-scrolled-${width}-${theme}`, shell, header, footer, body);
        const field = form === "model" ? page.getByLabel(/^连接名称/) : form === "expert" ? page.getByRole("textbox", { name: "专家名称", exact: true }) : page.getByPlaceholder("请输入项目名称", { exact: true });
        await field.fill("回归草稿");
        await page.keyboard.press("Escape");
        await page.getByRole("alertdialog", { name: "舍弃未保存的修改？" }).waitFor();
        await page.getByRole("button", { name: "继续编辑", exact: true }).click();
        assert.equal(await field.inputValue(), "回归草稿", `${form}: draft lost after canceling discard`);
      }
    }
  }
  if (!baseline) {
    await page.setViewportSize({ width: 1024, height: 680 });
    for (const state of ["loading", "empty", "load-error", "save-error"]) {
      await page.goto(`http://127.0.0.1:1439/__ui-review?surface=model&modelState=${state}&theme=dark`);
      await page.getByRole("button", { name: "同步模型", exact: true }).click();
      const dialog = page.getByRole("dialog", { name: "同步模型", exact: true });
      await dialog.waitFor();
      if (state === "loading") await dialog.getByRole("status").waitFor();
      if (state === "empty") await dialog.getByText("服务端没有返回模型，请关闭后使用“手动添加”。").waitFor();
      if (state === "load-error") await dialog.getByRole("alert").waitFor();
      if (state === "save-error") {
        await dialog.getByRole("checkbox").first().check();
        await dialog.getByRole("button", { name: "添加 1 个模型", exact: true }).click();
        await dialog.getByRole("alert").scrollIntoViewIfNeeded();
        assert.equal(await dialog.getByRole("checkbox").count(), 36, "save failure hides the editable model list");
        assert.ok(await dialog.getByRole("checkbox").first().isChecked(), "save failure discards the selected model");
      }
      await inspect(`model-import-${state}-1024-dark`, ".models-settings-panel__editor", ".models-settings-panel__editor-header", ".models-settings-panel__editor-footer", ".models-settings-panel__editor-body");
      if (state === "load-error") {
        await dialog.getByRole("button", { name: "重试获取模型", exact: true }).click();
        await dialog.getByRole("checkbox").first().waitFor();
        assert.equal(await dialog.getByRole("alert").count(), 0, "successful retry retains the load error");
      }
      if (state === "save-error") {
        await dialog.getByRole("checkbox").first().uncheck();
        await dialog.getByRole("checkbox").nth(1).check();
        assert.equal(await dialog.getByRole("alert").count(), 0, "editing after failure retains a stale save error");
        assert.ok(await dialog.getByRole("button", { name: "添加 1 个模型", exact: true }).isEnabled());
      }
    }
    await page.setViewportSize({ width: 1024, height: 680 });
    await page.goto("http://127.0.0.1:1439/__ui-review?surface=modal-long&theme=dark");
    await page.locator(".app-dialog").waitFor();
    await page.evaluate(() => { document.documentElement.style.fontSize = "22px"; });
    await inspect("modal-long-scaled-1024-dark", ".app-dialog", ".app-dialog__header", ".app-dialog__actions", ".app-dialog__body");
    await page.goto("http://127.0.0.1:1439/__ui-review?surface=details&detailVariant=knowledge&theme=dark");
    await page.getByRole("button", { name: /知识来源/ }).press("ArrowDown");
    const menu = page.getByRole("menu", { name: "选择知识来源" });
    await menu.waitFor();
    const box = await menu.boundingBox();
    assert.ok(box.x >= 0 && box.y >= 0 && box.x + box.width <= 1024 && box.y + box.height <= 680, "knowledge menu clips");
    await page.screenshot({ path: join(output, "knowledge-menu-1024-dark.png") });
    await page.keyboard.press("Escape");
    await menu.waitFor({ state: "hidden" });
  }
  const results = await validateResultReview(page, output, baseline);
  const execution = baseline ? null : await validateExecutionReview(page, output);
  if (!baseline) assert.deepEqual(errors, [], "browser runtime errors");
  writeFileSync(join(output, "dialog-review.json"), JSON.stringify({ passed: !baseline, baseline, errors, checks, results, execution }, null, 2));
  console.log(JSON.stringify({ baseline, dialogs: checks.length, results, execution, errors, output }));
} finally { await browser.close(); await server.close(); }
