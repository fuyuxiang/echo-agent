import assert from "node:assert/strict";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createUiReviewServer, reviewChromium, reviewOutput } from "./ui-review-runtime.mjs";

const output = reviewOutput(mkdtempSync(join(tmpdir(), "echo-select-ui-")));
console.log(`Screenshots: ${output}`);
const server = await createUiReviewServer(0, { fixture: "/scripts/fixtures/expert-project-review.tsx" });
let browser, surfaces;
try {
  browser = await (await reviewChromium()).launch({ headless: true });
  for (const [width, height, theme] of [[1280, 800, "light"], [1280, 800, "dark"], [420, 900, "light"]]) {
    const page = await browser.newPage({ viewport: { width, height } });
    page.setDefaultNavigationTimeout(90000);
    const errors = [];
    page.on("pageerror", error => errors.push(error.message));
    const url = `http://127.0.0.1:${server.httpServer.address().port}/__ui-review?theme=${theme}&organization=offline`;
    try {
      await page.goto(url);
      const scope = page.getByRole("combobox", { name: "项目范围" });
      await scope.waitFor();
      await page.waitForFunction(() => window.reviewCalls?.some(call => call.command === "org_project_request"));
      await page.getByRole("button", { name: "打开项目 产品需求梳理", exact: true }).waitFor();
      assert.equal(await page.locator(".project-connection-notice").count(), 0);
      assert.equal(await scope.evaluate(field => getComputedStyle(field).appearance), "none");
      await page.screenshot({ path: join(output, `${width}-${theme}-projects.png`) });
      await scope.click();
      const popup = page.getByRole("listbox", { name: "项目范围" });
      await popup.waitFor();
      const bounds = await popup.boundingBox();
      assert(bounds.x >= 0 && bounds.y >= 0 && bounds.x + bounds.width <= width + 1 && bounds.y + bounds.height <= height + 1);
      await page.screenshot({ path: join(output, `${width}-${theme}-select.png`) });
      await popup.getByRole("option", { name: "已共享", exact: true }).click();
      await page.getByText("暂时无法连接组织服务", { exact: true }).waitFor();
      assert.equal(await page.getByText("技术详情", { exact: true }).evaluate(summary => summary.parentElement.open), false);
      await page.screenshot({ path: join(output, `${width}-${theme}-shared-offline.png`) });
      await scope.focus();
      await page.keyboard.press("Home");
      await page.keyboard.press("Enter");
      assert.equal(await scope.inputValue(), "all");
      assert.equal(await page.locator(".project-connection-notice").count(), 0);
      await scope.click();
      await page.getByRole("listbox").getByRole("option", { name: "仅自己", exact: true }).click();
      assert.equal(await page.locator(".project-connection-notice").count(), 0);

      // The popup must remain above the actual project dialog and Escape may
      // dismiss only the popup, preserving the dialog and its form focus.
      await page.getByRole("button", { name: "新建项目", exact: true }).click();
      const dialog = page.getByRole("dialog", { name: "新建项目", exact: true });
      await dialog.locator("summary").filter({ hasText: "项目指令与模板" }).click();
      const template = dialog.getByRole("combobox", { name: "选择项目指令模板" });
      await template.click();
      const options = page.getByRole("listbox", { name: "选择项目指令模板" });
      await options.waitFor();
      assert.equal(await options.evaluate(menu => {
        const row = menu.querySelector('[role="option"]');
        const rect = row.getBoundingClientRect();
        return menu.contains(document.elementFromPoint(rect.x + rect.width / 2, rect.y + rect.height / 2));
      }), true, "dialog must not cover its selection popup");
      await page.screenshot({ path: join(output, `${width}-${theme}-dialog-select.png`) });
      await page.keyboard.press("Escape");
      assert.equal(await page.getByRole("listbox").count(), 0);
      assert.equal(await dialog.count(), 1);
      await page.keyboard.press("Escape");
      assert.equal(await dialog.count(), 0);

      await page.goto(`${url}&linked=1`);
      await page.getByRole("button", { name: "产品需求梳理 更多操作", exact: true }).waitFor();
      assert.equal(await page.locator(".project-connection-notice").count(), 0);
      await scope.click();
      await page.getByRole("listbox").getByRole("option", { name: "已共享", exact: true }).click();
      await page.getByText(/当前共享列表可能不完整/).waitFor();
      await page.getByRole("button", { name: "打开项目 产品需求梳理" }).click();
      await page.getByText("暂时无法连接组织服务", { exact: true }).waitFor();
      assert.equal(await page.getByRole("button", { name: "问项目", exact: true }).isEnabled(), false);
      await page.getByRole("tab", { name: "资料与成果" }).click();
      const materials = page.getByRole("tabpanel", { name: "资料与成果" });
      await materials.getByRole("button", { name: "需求说明.md", exact: true }).waitFor();
      await page.screenshot({ path: join(output, `${width}-${theme}-shared-project-offline.png`) });
      await page.evaluate(() => window.reviewSetOrganizationOffline(false));
      await page.getByRole("button", { name: "重试", exact: true }).click();
      await page.getByText("已同步", { exact: true }).waitFor();
      assert.equal(await page.getByRole("button", { name: "问项目", exact: true }).isEnabled(), true);

      await page.goto(`${url.replace("organization=offline", "organization=online")}&linked=1&linkedDeleted=1`);
      await page.getByRole("button", { name: "打开项目 产品需求梳理" }).click();
      await page.getByText("共享项目已删除", { exact: true }).waitFor();
      await page.getByRole("tab", { name: "资料与成果" }).click();
      await materials.getByRole("button", { name: "需求说明.md", exact: true }).waitFor();
      await page.getByRole("button", { name: "项目", exact: true }).click();
      await page.getByRole("listitem").filter({ hasText: "产品需求梳理" }).getByText("仅自己", { exact: true }).waitFor();

      await page.goto(`${url}&empty=1`);
      await page.getByText("还没有本机项目", { exact: true }).waitFor();
      assert.equal(await page.locator(".project-connection-notice").count(), 0);
      await scope.click();
      await page.getByRole("listbox").getByRole("option", { name: "已共享", exact: true }).click();
      await page.getByText("暂时无法连接组织服务", { exact: true }).waitFor();
      await page.evaluate(() => window.reviewSetOrganizationOffline(false));
      await page.getByRole("button", { name: "重试", exact: true }).click();
      await page.getByRole("button", { name: "打开项目 客户交付", exact: true }).waitFor();
      assert.equal(await page.locator(".project-connection-notice").count(), 0);
      assert.equal(await page.getByRole("button", { name: "客户交付 更多操作", exact: true }).count(), 0);
      await page.goto(`${url.replace("organization=offline", "organization=online")}&empty=1&remoteAdmin=1`);
      await page.getByRole("button", { name: "客户交付 更多操作", exact: true }).click();
      const projectMenu = page.getByRole("menu", { name: "客户交付 操作菜单" });
      assert.deepEqual(await projectMenu.getByRole("menuitem").allTextContents(), ["进入项目", "重命名", "删除共享项目"]);
      assert.equal(await page.evaluate(() => window.reviewCalls.filter(call => call.args?.action === "snapshot").length), 0);
      await page.screenshot({ path: join(output, `${width}-${theme}-shared-project-menu.png`) });
      await page.keyboard.press("Escape");
      await page.evaluate(() => {
        window.reviewSetOrganizationOffline(true);
        window.dispatchEvent(new Event("focus"));
      });
      await page.waitForFunction(() => window.reviewCalls.filter(call => call.args?.action === "list").length >= 2);
      assert.equal(await page.locator(".project-connection-notice").count(), 0);
      await page.getByRole("button", { name: "打开项目 客户交付", exact: true }).click();
      const remoteCard = page.getByRole("listitem").filter({ hasText: "客户交付" });
      await remoteCard.getByText("暂时无法连接组织服务", { exact: true }).waitFor();
      const noticeFitsCard = () => remoteCard.evaluate(card => {
        const row = card.getBoundingClientRect();
        const notice = card.querySelector(".project-connection-notice").getBoundingClientRect();
        return notice.left >= row.left && notice.right <= row.right && notice.top >= row.top && notice.bottom <= row.bottom;
      });
      assert.equal(await noticeFitsCard(), true, "the connection notice should fit inside its card");
      await page.getByRole("button", { name: "列表视图", exact: true }).click();
      assert.equal(await noticeFitsCard(), true, "the connection notice should also fit inside its list row");
      await page.getByRole("button", { name: "卡片视图", exact: true }).click();
      await page.evaluate(() => window.reviewSetOrganizationOffline(false));
      await page.getByRole("button", { name: "客户交付 更多操作", exact: true }).click();
      await projectMenu.getByRole("menuitem", { name: "删除共享项目" }).click();
      const deleteConfirmation = page.getByRole("alertdialog", { name: "删除共享项目「客户交付」？" });
      assert.match(await deleteConfirmation.innerText(), /所有成员都会失去/);
      await deleteConfirmation.getByRole("button", { name: "删除共享项目", exact: true }).click();
      await page.getByRole("button", { name: "打开项目 客户交付", exact: true }).waitFor({ state: "hidden" });
      await deleteConfirmation.waitFor({ state: "hidden" });
      assert.equal(await page.evaluate(() => window.reviewCalls.filter(call => call.args?.action === "delete").length), 1);

      await page.goto(`${url.replace("organization=offline", "organization=online")}&linked=1`);
      await page.getByRole("listitem").filter({ hasText: "产品需求梳理" }).getByText("已共享 · 管理员", { exact: true }).waitFor();
      await page.getByRole("button", { name: "产品需求梳理 更多操作", exact: true }).click();
      await page.getByRole("menuitem", { name: "删除共享项目", exact: true }).click();
      const linkedDeletion = page.getByRole("alertdialog", { name: "删除共享项目「产品需求梳理」？" });
      assert.match(await linkedDeletion.innerText(), /资料副本会保留/);
      await linkedDeletion.getByRole("button", { name: "删除共享项目", exact: true }).click();
      await linkedDeletion.waitFor({ state: "hidden" });
      await page.getByRole("button", { name: "打开项目 产品需求梳理", exact: true }).click();
      await page.getByText("共享项目已删除", { exact: true }).waitFor();
      await page.getByRole("tab", { name: "资料与成果" }).click();
      await materials.getByRole("button", { name: "需求说明.md", exact: true }).waitFor();
      assert.equal(await page.evaluate(() => window.reviewCalls.some(call => call.command === "project_assets_remove_all")), false);

      await page.goto(`${url}&surface=knowledge`);
      await page.getByRole("button", { name: "组织知识", exact: true }).click();
      const knowledgeScope = page.getByRole("combobox", { name: "组织知识范围", exact: true });
      await knowledgeScope.click();
      await page.getByRole("listbox").getByRole("option", { name: "全组织 · 全公司", exact: true }).click();
      assert.equal(await knowledgeScope.inputValue(), "org");
      assert.equal(await page.getByRole("menu", { name: "选择知识来源" }).count(), 1);
      await page.getByText("本任务只检索“全公司”", { exact: true }).waitFor();
      assert.equal(await page.evaluate(() => window.reviewCalls.some(call => call.command === "agent_set_knowledge_sources" && call.args.organizationScopeIds?.[0] === "org")), true);
      await knowledgeScope.click();
      await page.keyboard.press("Escape");
      assert.equal(await page.getByRole("listbox").count(), 0);
      assert.equal(await page.getByRole("menu", { name: "选择知识来源" }).count(), 1);
      await page.screenshot({ path: join(output, `${width}-${theme}-nested-knowledge.png`) });
      await page.keyboard.press("Escape");
      assert.equal(await page.getByRole("menu").count(), 0);
      assert.deepEqual(errors, []);
      console.log(`PASS ${width}/${theme}: incomplete catalogs and recovery; nested pointer selection; modal layering and Escape`);
    } catch (error) {
      await page.screenshot({ path: join(output, `${width}-${theme}-failure.png`) });
      console.error(await page.getByRole("dialog").allTextContents());
      throw error;
    } finally { await page.close(); }
  }
  await server.close();
  surfaces = await createUiReviewServer(0);
  const base = `http://127.0.0.1:${surfaces.httpServer.address().port}/__ui-review`;
  for (const [width, height, theme] of [[1280, 800, "light"], [1024, 768, "dark"], [420, 900, "light"]]) {
    const page = await browser.newPage({ viewport: { width, height } });
    page.setDefaultNavigationTimeout(90000);
    const errors = [];
    page.on("pageerror", error => errors.push(error.message));
    try {
      await page.goto(`${base}?surface=model&modelSettingsReview=1&theme=${theme}`);
      const model = page.getByRole("combobox", { name: "默认模型", exact: true });
      await model.waitFor();
      assert.equal(await model.evaluate(field => field.getBoundingClientRect().height), 48);
      assert.equal(await page.locator(".model-settings__select-shell > svg").count(), 0);
      assert.notEqual(await model.evaluate(field => getComputedStyle(field).backgroundImage), "none");
      await model.click();
      await page.getByRole("listbox").getByRole("option", { name: /备用模型/ }).click();
      await page.getByRole("status").filter({ hasText: "已将「备用模型」设为默认模型" }).waitFor();
      await page.screenshot({ path: join(output, `${width}-${theme}-model-size.png`) });
      await page.emulateMedia({ forcedColors: "active" });
      assert.equal(await model.evaluate(field => getComputedStyle(field).appearance), "auto");
      assert.equal(await model.evaluate(field => getComputedStyle(field).backgroundImage), "none");
      await page.emulateMedia({ forcedColors: "none" });
      await page.goto(`${base}?surface=organization&orgState=many&theme=${theme}`);
      await page.locator(".org-memory__nav button").filter({ hasText: "文档" }).click();
      const organizationScope = page.getByRole("combobox", { name: "查看范围", exact: true });
      await organizationScope.waitFor();
      assert.equal(await page.locator(".org-resource-select > svg").count(), 0);
      assert.notEqual(await organizationScope.evaluate(field => getComputedStyle(field).backgroundImage), "none");
      await organizationScope.click();
      await page.getByRole("listbox").getByRole("option", { name: /团队/ }).first().click();
      assert.notEqual(await organizationScope.inputValue(), "");
      await page.screenshot({ path: join(output, `${width}-${theme}-organization-select.png`) });
      assert.deepEqual(errors, []);
      console.log(`PASS ${width}/${theme}: model dimensions and one indicator; model saving and organization filtering`);
    } catch (error) {
      await page.screenshot({ path: join(output, `${width}-${theme}-surface-failure.png`) });
      throw error;
    } finally { await page.close(); }
  }
} finally { await browser?.close(); await surfaces?.close(); await server.close(); }
